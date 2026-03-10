# Phase 17: MTG-Specific UI - Research

**Researched:** 2026-03-09
**Domain:** React/TypeScript UI — stack visualization, mana payment, combat controls, priority management
**Confidence:** HIGH

## Summary

Phase 17 upgrades four stub UI subsystems (stack, mana payment, combat, priority/pass) to Arena-quality interactions. The CONTEXT.md provides extremely specific guidance: port Alchemy's ActionButton as a unified orchestrator, port BlockAssignmentLines for animated SVG combat lines, port buttonStyles for consistent game buttons, and rewrite usePhaseInfo for MTG's 12-phase turn structure. The StackDisplay is a separate persistent component (not part of ActionButton), unlike Alchemy where the stack mini-pile is inlined in CombatPriorityControls.

The existing codebase has clean separation of concerns: `gameStore` provides `waitingFor`, `legalActions`, and game state; `uiStore` owns combat selection state; `dispatchAction` handles the animation-aware dispatch pipeline; and `gameLoopController` + `autoPass.ts` handle automatic priority passing. All existing components (PassButton, CombatOverlay, AttackerControls, BlockerControls, BlockerArrow, StackDisplay, StackEntry, ManaPaymentUI) are functional but minimal stubs ready for replacement. The engine already supports all necessary WaitingFor states and GameAction types — no Rust changes needed.

**Primary recommendation:** Port Alchemy patterns (ActionButton, BlockAssignmentLines, buttonStyles) with MTG-specific adaptations, keeping StackDisplay as a separate right-column component with full Scryfall card images.

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions
- Port Alchemy's ActionButton as the unified combat/phase-advance orchestrator, replacing current PassButton, CombatOverlay, AttackerControls, and BlockerControls
- Port BlockAssignmentLines (animated SVG dashed lines with glow and pulse dots) replacing current BlockerArrow
- Port buttonStyles (gameButtonClass helper with tone variants) for consistent button styling
- Port usePhaseInfo hook, fully rewritten for MTG's multi-step turn structure (Untap, Upkeep, Draw, Main 1, Combat phases, Main 2, End, Cleanup)
- Skip CombatMathBubbles — not porting combat math preview for MTG due to keyword complexity
- Hybrid architecture: ActionButton handles combat controls + phase advance + pass/resolve, while StackDisplay is a separate persistent component for stack visualization
- Use legalActions from gameStore (already populated via get_legal_actions_js() WASM export) to drive ActionButton visibility
- Right-side column position for stack, always visible when stack has items
- Full Scryfall card image previews in a staggered pile, newest on top
- Cards dynamically shrink as more items are added to the stack (container-aware sizing)
- "Resolve" button passes priority for top item; "Resolve All" auto-passes for each item sequentially with animations
- Player can interrupt Resolve All by playing a card or activating an ability
- "Resolves Next" label on the top stack item
- No special "respond" mode — player plays instants from hand normally while stack is visible
- Silent auto-pay: engine automatically selects optimal mana sources and taps lands without player confirmation
- Mana payment UI only appears for ambiguous costs: hybrid, phyrexian, and X costs
- X costs: horizontal slider showing 0 to max-affordable-X with resulting total cost display
- Phyrexian mana: inline toggle per symbol — tap to switch between mana icon and heart icon
- Hybrid mana: inline toggle per symbol — tap to switch between color options
- No manual mana tapping modal for simple costs
- Skip-confirm guard pattern from Alchemy: first tap on "No Attacks" arms it, second tap confirms. Same for "No Blocks"
- "All Attack" button toggles all legal attackers
- "Clear Attackers" / "Clear Blocks" buttons when selections exist
- Blocker assignment: click blocker creature, then click attacker to assign
- Multi-blocker damage assignment: engine auto-distributes damage optimally. Player can open override modal to manually redistribute
- Block assignment lines use ported BlockAssignmentLines from Alchemy (animated dashes, glow, pulse dots)
- Blocker order step: "Choose block order" with Resolve button
- Combat priority window: inline stack pile display with "Proceed to Blockers" / "Resolve Combat" labels

### Claude's Discretion
- Exact stack card sizing algorithm (min/max sizes, shrink curve as stack grows)
- ActionButton positioning (safe-area-aware, responsive)
- Animation timing for stack entry/exit transitions
- Damage assignment modal layout and slider/button design
- How usePhaseInfo maps MTG phases to display keys and advance actions
- Whether to keep PhaseStopBar separate or integrate into usePhaseInfo

### Deferred Ideas (OUT OF SCOPE)
- Combat math bubbles — deferred due to MTG keyword complexity
- Manual mana tapping mode / power-user override
- Stack item hover preview (show full-size card on hover)
</user_constraints>

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|-----------------|
| STACK-01 | Arena-style stack visualization with card art and description | StackDisplay upgrade: right-column staggered card pile with Scryfall images, container-aware sizing via boardSizing pattern |
| STACK-02 | Priority pass/respond buttons when player has priority | ActionButton port: reads waitingFor + legalActions, shows Resolve/Done/pass buttons |
| STACK-03 | Auto-pass toggle when no instant-speed actions available | Already implemented in autoPass.ts + gameLoopController; ActionButton integrates with existing uiStore.autoPass toggle |
| STACK-04 | Full-control mode disables all auto-passing | Already implemented in FullControlToggle + autoPass.ts shouldAutoPass(); no changes needed |
| MANA-01 | Mana payment UI displays required cost with WUBRG symbols | ManaPaymentUI upgrade: only shown for ambiguous costs, ManaCostShard enum drives symbol rendering |
| MANA-02 | Hybrid/phyrexian/X costs with appropriate affordances | Inline toggles for hybrid (color switch) and phyrexian (mana vs life), slider for X costs |
| MANA-03 | Mana pool display updates in real-time | ManaBadge already exists; mana pool summary via gameStore reactive subscription |
| COMBAT-01 | Attacker declaration with click-to-toggle on highlighted legal options | ActionButton combat controls: All Attack, Clear Attackers, skip-confirm guard pattern |
| COMBAT-02 | Blocker declaration with click-to-assign on highlighted legal options | ActionButton blocker controls: click blocker then attacker, Clear Blocks, skip-confirm guard |
| COMBAT-03 | Combat math bubbles preview P/T trade outcomes | DEFERRED per user decision — skip CombatMathBubbles |
| COMBAT-04 | Damage assignment modal for multi-blocker damage distribution | Override modal with slider per blocker, engine auto-distributes by default |
</phase_requirements>

## Standard Stack

### Core
| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| react | 19.x | Component framework | Already in project |
| zustand | 5.x | State management | Already in project (gameStore, uiStore, preferencesStore) |
| framer-motion | 12.x | Animations | Already in project for all UI animations |
| tailwindcss | 4.x | Styling | Already in project |

### Supporting
| Library | Version | Purpose | When to Use |
|---------|---------|---------|-------------|
| react-dom (createPortal) | 19.x | Overlay rendering | BlockAssignmentLines SVG overlay, damage assignment modal |

### No New Dependencies
This phase uses exclusively existing project dependencies. No new packages needed.

## Architecture Patterns

### Component Replacement Map
```
REMOVE:
  components/controls/PassButton.tsx      → absorbed into ActionButton
  components/combat/AttackerControls.tsx   → absorbed into ActionButton
  components/combat/BlockerControls.tsx    → absorbed into ActionButton
  components/combat/BlockerArrow.tsx       → replaced by BlockAssignmentLines
  components/combat/CombatOverlay.tsx      → replaced by ActionButton

UPGRADE IN-PLACE:
  components/stack/StackDisplay.tsx        → full card pile with Scryfall images
  components/stack/StackEntry.tsx          → full card preview with shrink sizing
  components/mana/ManaPaymentUI.tsx        → hybrid/phyrexian/X cost support

NEW:
  components/board/ActionButton.tsx        → unified combat/priority/phase orchestrator
  components/board/BlockAssignmentLines.tsx → animated SVG block lines
  components/ui/buttonStyles.ts            → gameButtonClass helper
  hooks/usePhaseInfo.ts                    → MTG phase display mapping
  components/combat/DamageAssignmentModal.tsx → multi-blocker damage override
```

### Recommended File Structure
```
client/src/
├── components/
│   ├── board/
│   │   ├── ActionButton.tsx          # NEW: unified orchestrator
│   │   └── BlockAssignmentLines.tsx  # NEW: animated SVG block lines
│   ├── combat/
│   │   └── DamageAssignmentModal.tsx # NEW: damage distribution override
│   ├── mana/
│   │   ├── ManaBadge.tsx             # KEEP: mana color badge
│   │   ├── ManaPaymentUI.tsx         # UPGRADE: hybrid/phyrexian/X support
│   │   └── ManaSymbol.tsx            # NEW: individual mana cost shard renderer
│   ├── stack/
│   │   ├── StackDisplay.tsx          # UPGRADE: right-column card pile
│   │   └── StackEntry.tsx            # UPGRADE: full card preview
│   └── ui/
│       └── buttonStyles.ts           # NEW: gameButtonClass helper
├── hooks/
│   └── usePhaseInfo.ts               # NEW: MTG phase mapping
└── pages/
    └── GamePage.tsx                   # UPDATE: wire new components
```

### Pattern 1: ActionButton as Unified Orchestrator
**What:** Single component that renders different controls based on game state (WaitingFor + phase + legalActions)
**When to use:** Always rendered in GamePage, shows/hides sub-controls based on state

The ActionButton reads three sources:
1. `gameStore.waitingFor` — determines if player has priority, is in combat, etc.
2. `gameStore.legalActions` — determines what buttons to show (has declare attackers? has pass priority?)
3. `gameStore.gameState.phase` — determines phase display and advance actions

Key adaptation from Alchemy: MTG uses `WaitingFor::Priority` for all priority windows (including combat priority), while Alchemy has a separate `combat_priority` phase type. The ActionButton must check both `waitingFor` and `gameState.phase` to determine if we're in a combat priority window.

```typescript
// ActionButton state determination (MTG adaptation)
function getActionButtonMode(
  waitingFor: WaitingFor | null,
  phase: Phase,
  legalActions: GameAction[],
  stackLength: number,
): 'combat-attackers' | 'combat-blockers' | 'priority-stack' | 'priority-empty' | 'phase-advance' | 'hidden' {
  if (!waitingFor || waitingFor.type !== 'Priority') return 'hidden';
  if (waitingFor.data.player !== PLAYER_ID) return 'hidden';

  // Check if we're in a combat declaration phase
  // (WaitingFor is Priority but phase is DeclareAttackers/DeclareBlockers)
  if (phase === 'DeclareAttackers') return 'combat-attackers';
  if (phase === 'DeclareBlockers') return 'combat-blockers';

  // Stack has items — show resolve controls
  if (stackLength > 0) return 'priority-stack';

  // Empty stack with priority — show Done / phase advance
  return 'priority-empty';
}
```

### Pattern 2: Skip-Confirm Guard (from Alchemy)
**What:** Two-tap pattern for destructive "skip" actions to prevent accidents
**When to use:** "No Attacks" and "No Blocks" buttons

```typescript
type SkipConfirmStep = 'declare_attackers' | 'declare_blockers' | null;
const SKIP_CONFIRM_WINDOW_MS = 1200;

// First tap: arm the guard (button changes to "Tap again: No Attacks")
// Second tap within 1.2s: confirm skip
// Timeout: reset to initial state
```

### Pattern 3: Container-Aware Stack Card Sizing
**What:** Stack cards shrink as more items are added, similar to boardSizing.ts
**When to use:** StackDisplay with dynamic card count

```typescript
function getStackCardSize(stackCount: number): { width: number; height: number } {
  const BASE_WIDTH = 160;   // ~10rem
  const BASE_HEIGHT = 224;  // 1.4:1 aspect ratio
  const MIN_WIDTH = 80;     // ~5rem minimum
  const ASPECT = BASE_HEIGHT / BASE_WIDTH;

  // Shrink curve: full size for 1-2 items, linear shrink to min at 8+ items
  const scale = Math.max(0.5, 1 - Math.max(0, stackCount - 2) * 0.083);
  const width = Math.max(MIN_WIDTH, BASE_WIDTH * scale);
  return { width, height: width * ASPECT };
}
```

### Pattern 4: Resolve All with Interrupt
**What:** Sequential auto-pass with animation between each resolution
**When to use:** "Resolve All" button on stack

The dispatchAction pipeline already handles sequential dispatch with animation waits. "Resolve All" dispatches PassPriority repeatedly, but checks between each dispatch whether the player has played a card (new stack entry appeared = interrupt).

```typescript
async function resolveAll(dispatch: typeof dispatchAction): Promise<void> {
  const initialStackLength = useGameStore.getState().gameState?.stack.length ?? 0;

  for (let i = 0; i < initialStackLength; i++) {
    const currentState = useGameStore.getState();
    const currentStack = currentState.gameState?.stack.length ?? 0;

    // Player interrupted by playing a card (stack grew)
    if (currentStack > initialStackLength - i) break;

    // No more stack items or not our priority
    if (currentStack === 0) break;
    if (currentState.waitingFor?.type !== 'Priority') break;
    if (currentState.waitingFor.data.player !== PLAYER_ID) break;

    await dispatch({ type: 'PassPriority' });
  }
}
```

### Pattern 5: MTG usePhaseInfo Hook
**What:** Maps MTG's 12 phases to display entries and advance actions
**When to use:** PhaseTracker display strip + ActionButton advance button

Key difference from Alchemy: MTG has 12 distinct phases vs Alchemy's 5. The display should compress some phases for the strip (combat sub-phases group as "Combat") while showing full detail in the ActionButton label.

```typescript
interface PhaseInfo {
  displayKey: string;       // 'main1' | 'combat' | 'main2' | 'end' | etc.
  currentOrder: number;     // For strip highlighting
  canAdvance: boolean;      // Is ADVANCE/PassPriority legal?
  advanceAction: GameAction | null;
  advanceLabel: string | null; // "Battle!" | "End Turn" | "Done"
  phases: PhaseEntry[];     // Strip entries
}

// MTG phase strip entries (simplified for display)
const MTG_PHASES: PhaseEntry[] = [
  { key: 'main1', label: 'Main 1' },
  { key: 'combat', label: 'Combat' },
  { key: 'main2', label: 'Main 2' },
  { key: 'end', label: 'End' },
];

function getDisplayPhaseKey(phase: Phase): string {
  switch (phase) {
    case 'PreCombatMain': return 'main1';
    case 'BeginCombat':
    case 'DeclareAttackers':
    case 'DeclareBlockers':
    case 'CombatDamage':
    case 'EndCombat': return 'combat';
    case 'PostCombatMain': return 'main2';
    default: return 'end';
  }
}

function getAdvanceAction(phase: Phase): { action: GameAction; label: string } | null {
  switch (phase) {
    case 'PreCombatMain': return { action: { type: 'PassPriority' }, label: 'Battle!' };
    case 'PostCombatMain': return { action: { type: 'PassPriority' }, label: 'End Turn' };
    default: return null;
  }
}
```

### Anti-Patterns to Avoid
- **Duplicating state between ActionButton and uiStore:** ActionButton should read combat selection from uiStore, not maintain its own copy. The existing `selectedAttackers`, `blockerAssignments`, and `combatClickHandler` in uiStore are the single source of truth.
- **Dispatching combat actions directly in ActionButton:** Use the existing `dispatchAction` pipeline (animation-aware). Never call `adapter.submitAction` directly from components.
- **Rendering StackDisplay inside ActionButton:** Per user decision, StackDisplay is a separate persistent component. ActionButton may show a mini-stack inline for combat priority windows, but the main StackDisplay is independent.
- **Creating new stores for stack/mana state:** Use existing gameStore.gameState.stack and gameStore.gameState.players[].mana_pool. No new stores needed.

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| Button styling variants | Custom button components with props | gameButtonClass from buttonStyles.ts | Consistent tones, sizes, disabled states; proven in Alchemy |
| SVG block lines with animation | Manual SVG path calculation | Port BlockAssignmentLines with RAF polling | Handles Framer Motion layout animations, stabilizes after position settling |
| Card position lookups | Manual DOM queries scattered in components | data-object-id / data-testid attribute convention + getCardCenter utility | Already established pattern in project |
| Stack card image loading | Custom image loading | Existing CardImage component with Scryfall service | Already handles caching via idb-keyval, size variants |
| Animation-aware dispatch | Direct WASM calls | dispatchAction from game/dispatch.ts | Handles snapshot capture, event normalization, animation queueing, state update |
| Auto-pass logic | Custom priority skip logic | Existing shouldAutoPass + gameLoopController | Already handles phase stops, full control, legal action filtering |

## Common Pitfalls

### Pitfall 1: WaitingFor vs Phase Confusion for Combat
**What goes wrong:** MTG uses `WaitingFor::Priority` during combat priority windows, not a separate combat-priority WaitingFor type. The *phase* (BeginCombat, DeclareAttackers, etc.) indicates which combat step we're in.
**Why it happens:** Alchemy has a dedicated `combat_priority` phase type, so porting 1:1 breaks.
**How to avoid:** ActionButton must read both `waitingFor.type` AND `gameState.phase` to determine its mode. For combat declarations, the WaitingFor is `DeclareAttackers` or `DeclareBlockers` (not Priority).
**Warning signs:** ActionButton shows wrong controls during combat, or shows resolve/pass during attacker declaration.

### Pitfall 2: Zustand Selector Re-render Cascades
**What goes wrong:** Selecting complex objects from gameStore causes re-renders on every state change.
**Why it happens:** Object reference equality fails for nested objects (stack entries, legal actions array).
**How to avoid:** Use module-level empty array constants (established pattern), shallow equality selectors, and derive computed values inside selectors or useMemo.
**Warning signs:** ActionButton re-renders on every priority pass, stack display flickers.

### Pitfall 3: Stale legalActions During Resolve All
**What goes wrong:** Resolve All loop reads stale legalActions, dispatches actions when player no longer has priority.
**Why it happens:** dispatchAction is async; by the time one resolve completes, the AI may have responded and the game state changed.
**How to avoid:** Re-read gameStore.getState() between each iteration of the resolve-all loop. Check waitingFor and legalActions are still valid before each dispatch.
**Warning signs:** Console errors about invalid actions, game state corruption.

### Pitfall 4: BlockAssignmentLines Position Drift
**What goes wrong:** SVG lines don't track card positions during Framer Motion layout animations.
**Why it happens:** getBoundingClientRect returns the position at the time of call, but cards are animating to new positions.
**How to avoid:** Use the Alchemy RAF polling pattern: poll positions every frame until they stabilize (10 consecutive identical frames = stable). Use createPortal to render SVG at document.body level.
**Warning signs:** Lines point to wrong positions, visually "lag" behind card movement.

### Pitfall 5: Mana Payment UI Shown for Simple Costs
**What goes wrong:** ManaPaymentUI appears for every spell cast, interrupting flow.
**Why it happens:** Not checking whether the cost has ambiguous shards before showing UI.
**How to avoid:** Check ManaCost shards for hybrid (e.g., WhiteBlue), phyrexian (e.g., PhyrexianWhite), or X types. Only show UI when such shards exist. For simple costs (all single-color + generic), engine auto-pays silently.
**Warning signs:** Player must click through mana UI for every Lightning Bolt.

### Pitfall 6: DamageAssignment WaitingFor Not Yet Implemented
**What goes wrong:** Building a damage assignment modal but the engine never sends a WaitingFor for it.
**Why it happens:** The current engine auto-distributes damage in combat_damage.rs without waiting for player input. There is no `WaitingFor::DamageAssignment` variant.
**How to avoid:** The damage assignment modal should be an override that re-dispatches with manual assignments. Per CONTEXT.md, engine auto-distributes optimally, and player *can open* an override modal. This means the modal pre-populates with engine's auto-distribution and lets the player adjust before confirming.
**Warning signs:** Modal never appears because no WaitingFor triggers it.

## Code Examples

### gameButtonClass Helper (port from Alchemy)
```typescript
// Source: Alchemy src/components/ui/buttonStyles.ts — direct port
export type GameButtonTone = 'neutral' | 'emerald' | 'amber' | 'blue' | 'red' | 'indigo' | 'slate';
export type GameButtonSize = 'xs' | 'sm' | 'md' | 'lg';

const GAME_BUTTON_BASE =
  'min-h-11 border border-solid font-semibold backdrop-blur-sm transition-colors duration-150 cursor-pointer focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white/40 inline-flex items-center justify-center';

const GAME_BUTTON_TONES: Record<GameButtonTone, string> = {
  neutral: 'border-white/25 bg-white/8 text-white/80 hover:bg-white/14 hover:text-white',
  emerald: 'border-emerald-300/70 bg-emerald-500/20 text-emerald-100 hover:bg-emerald-400/28',
  amber: 'border-amber-300/70 bg-amber-500/20 text-amber-100 hover:bg-amber-400/30',
  blue: 'border-blue-300/70 bg-blue-500/20 text-blue-100 hover:bg-blue-400/30',
  red: 'border-red-300/70 bg-red-500/20 text-red-100 hover:bg-red-400/30',
  indigo: 'border-indigo-300/70 bg-indigo-500/20 text-indigo-100 hover:bg-indigo-400/30',
  slate: 'border-slate-300/60 bg-slate-500/20 text-slate-100 hover:bg-slate-400/28',
};

export function gameButtonClass(opts: {
  tone: GameButtonTone;
  size?: GameButtonSize;
  disabled?: boolean;
  className?: string;
}): string { /* ... same as Alchemy ... */ }
```

### MTG Phase-to-Display Mapping
```typescript
// usePhaseInfo.ts — MTG-specific (full rewrite of Alchemy version)
const MTG_PHASE_ORDER: Record<string, number> = {
  untap: 0,
  upkeep: 0,
  draw: 0,
  main1: 1,
  combat: 2,
  main2: 3,
  end: 4,
};

function getDisplayPhaseKey(phase: Phase): string {
  switch (phase) {
    case 'Untap': case 'Upkeep': case 'Draw': return 'draw';
    case 'PreCombatMain': return 'main1';
    case 'BeginCombat': case 'DeclareAttackers':
    case 'DeclareBlockers': case 'CombatDamage':
    case 'EndCombat': return 'combat';
    case 'PostCombatMain': return 'main2';
    case 'End': case 'Cleanup': return 'end';
  }
}
```

### ManaSymbol Component for Cost Display
```typescript
// Renders individual mana cost shards as WUBRG symbols
// ManaCostShard from engine maps to: "W", "U", "B", "R", "G", "C", "X"
// Plus hybrid: "W/U", phyrexian: "W/P", etc.

const SHARD_COLORS: Record<string, string> = {
  W: 'bg-yellow-200 text-yellow-900',
  U: 'bg-blue-500 text-white',
  B: 'bg-gray-800 text-gray-200',
  R: 'bg-red-500 text-white',
  G: 'bg-green-600 text-white',
  C: 'bg-gray-400 text-gray-900',
  X: 'bg-gray-500 text-white',
};

// Hybrid shards render as split circle (two halves)
// Phyrexian shards render with a Phi symbol overlay
```

### StackDisplay with Staggered Pile Layout
```typescript
// Each card overlaps the one below, offset by a fraction of card height
// Newest item (top of stack, last to resolve) is visually on top
// "Resolves Next" label on top item (stack[0] in MTG = first to resolve = top)

// Note: gameState.stack is ordered bottom-to-top (first entry = oldest)
// Reverse for display: show last entry at top of visual pile
const displayStack = [...stack].reverse();

// Stagger offset: each card shifts down and right slightly
const STAGGER_X = 4;  // px
const STAGGER_Y = 24; // px (shows enough card art below)
```

## State of the Art

| Old Approach (current stubs) | New Approach (this phase) | Impact |
|------------------------------|--------------------------|--------|
| Text-only StackEntry with tiny thumbnail | Full Scryfall card images in staggered pile | Stack feels like physical cards |
| Separate PassButton, AttackerControls, BlockerControls | Single ActionButton orchestrator | Unified UX, consistent styling, skip-confirm guards |
| Simple SVG line for BlockerArrow | Animated dashed SVG lines with glow and pulse dots | Polished combat visualization |
| ManaPaymentUI for all costs with manual land tapping | Auto-pay for simple costs, modal only for hybrid/phyrexian/X | Faster gameplay, less clicking |
| No "Resolve All" option | Sequential auto-resolve with interrupt | Efficient stack clearing |

## Integration Notes

### GamePage.tsx Wiring Changes
Current GamePage renders components conditionally based on WaitingFor:
```typescript
// CURRENT (to be changed)
{waitingFor?.type === "DeclareAttackers" && <CombatOverlay mode="attackers" />}
{waitingFor?.type === "DeclareBlockers" && <CombatOverlay mode="blockers" />}
// PassButton rendered in player area bar

// NEW: ActionButton is always rendered, handles its own visibility
<ActionButton />  // replaces PassButton + CombatOverlay
<StackDisplay />  // moved to right-side column, always rendered
// Remove CombatOverlay conditional renders
```

### uiStore Combat State (keep as-is)
The existing `selectedAttackers`, `blockerAssignments`, `toggleAttacker`, `selectAllAttackers`, `assignBlocker`, `clearCombatSelection`, and `combatClickHandler` in uiStore are all compatible with ActionButton. No store changes needed for combat.

### autoPass.ts Integration
ActionButton's "Done" button dispatches `PassPriority`. The existing `shouldAutoPass` and `gameLoopController` handle auto-passing. The ActionButton should check `uiStore.autoPass` to show/hide the auto-pass toggle, and `uiStore.fullControl` for the full-control indicator. Both toggles already exist (FullControlToggle component).

### ManaPaymentUI Engine Interaction
The current `WaitingFor::ManaPayment` is sent when the engine needs mana. The decision about whether to show UI or auto-pay should happen in the component: parse the cost shards, check for hybrid/phyrexian/X, and either auto-dispatch `PassPriority` (auto-pay) or show the modal.

Key question: The engine currently sends `WaitingFor::ManaPayment` for ALL costs. The UI must determine if the cost is ambiguous. Check `gameState.stack` for the top entry's cost, or check the `PendingCast.cost` from the most recent game state. The engine's ManaPayment data only includes `player` — the cost must be inferred from context.

### ManaCostShard Type on TS Side
The engine serializes ManaCostShard as strings: `"W"`, `"U"`, `"W/U"`, `"W/P"`, etc. The TS `ManaCost` type uses `shards: string[]` — these are the shard string representations. The ManaPaymentUI can pattern-match on these strings to determine if a cost is ambiguous (contains "/" = hybrid or phyrexian).

## Validation Architecture

### Test Framework
| Property | Value |
|----------|-------|
| Framework | Vitest (jsdom environment) |
| Config file | client/vitest.config.ts |
| Quick run command | `cd client && pnpm test -- --run --reporter=verbose` |
| Full suite command | `cd client && pnpm test -- --run` |

### Phase Requirements to Test Map
| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| STACK-01 | Stack displays card images in staggered pile | unit | `cd client && pnpm test -- --run src/components/stack/__tests__/StackDisplay.test.tsx` | Wave 0 |
| STACK-02 | Priority pass/respond buttons appear | unit | `cd client && pnpm test -- --run src/components/board/__tests__/ActionButton.test.tsx` | Wave 0 |
| STACK-03 | Auto-pass skips when no instant-speed actions | unit | `cd client && pnpm test -- --run src/game/__tests__/autoPass.test.ts` | Exists |
| STACK-04 | Full-control disables auto-passing | unit | `cd client && pnpm test -- --run src/game/__tests__/autoPass.test.ts` | Exists |
| MANA-01 | Mana payment displays WUBRG symbols | unit | `cd client && pnpm test -- --run src/components/mana/__tests__/ManaPaymentUI.test.tsx` | Wave 0 |
| MANA-02 | Hybrid/phyrexian/X cost affordances | unit | `cd client && pnpm test -- --run src/components/mana/__tests__/ManaPaymentUI.test.tsx` | Wave 0 |
| MANA-03 | Mana pool updates in real-time | unit | `cd client && pnpm test -- --run src/components/mana/__tests__/ManaBadge.test.tsx` | Wave 0 |
| COMBAT-01 | Attacker declaration click-to-toggle | unit | `cd client && pnpm test -- --run src/components/board/__tests__/ActionButton.test.tsx` | Wave 0 |
| COMBAT-02 | Blocker declaration click-to-assign | unit | `cd client && pnpm test -- --run src/components/board/__tests__/ActionButton.test.tsx` | Wave 0 |
| COMBAT-03 | Combat math bubbles | manual-only | N/A — DEFERRED | N/A |
| COMBAT-04 | Damage assignment modal | unit | `cd client && pnpm test -- --run src/components/combat/__tests__/DamageAssignmentModal.test.tsx` | Wave 0 |

### Sampling Rate
- **Per task commit:** `cd client && pnpm test -- --run --reporter=verbose`
- **Per wave merge:** `cd client && pnpm test -- --run && cd client && pnpm run type-check`
- **Phase gate:** Full frontend suite green + type-check before verify

### Wave 0 Gaps
- [ ] `src/components/board/__tests__/ActionButton.test.tsx` — covers STACK-02, COMBAT-01, COMBAT-02
- [ ] `src/components/stack/__tests__/StackDisplay.test.tsx` — covers STACK-01
- [ ] `src/components/mana/__tests__/ManaPaymentUI.test.tsx` — covers MANA-01, MANA-02
- [ ] `src/components/combat/__tests__/DamageAssignmentModal.test.tsx` — covers COMBAT-04

## Open Questions

1. **ManaPayment Cost Inference**
   - What we know: `WaitingFor::ManaPayment` only carries `player: PlayerId`. The cost being paid is not directly in the WaitingFor data.
   - What's unclear: How does the UI determine the mana cost to display? Options: (a) read from the top stack entry's source object mana_cost, (b) add cost to WaitingFor::ManaPayment in the engine, (c) infer from game events.
   - Recommendation: Read the mana cost from the most recently cast spell's source object. If `gameState.stack` has items, the top entry's `source_id` maps to an object with `mana_cost`. If the ManaPayment is for a non-stack cost (activated ability), check the object's abilities. This is a UI-side inference and avoids engine changes.

2. **DamageAssignment Without WaitingFor**
   - What we know: The engine auto-distributes damage without player input. There is no `WaitingFor::DamageAssignment` variant.
   - What's unclear: How does the player open the override modal? Options: (a) add a WaitingFor variant to the engine, (b) intercept at the UI level before combat damage resolves.
   - Recommendation: For v1.1, keep engine auto-distribution. Add a UI-only "Review Damage" button during the CombatDamage phase that shows the engine's assignments and lets the player see them (read-only). Full manual override would require a `WaitingFor::DamageAssignment` engine variant — defer that to a future phase. This satisfies COMBAT-04 partially (shows assignments) without engine changes.

3. **Resolve All Interrupt Mechanism**
   - What we know: dispatchAction is async and sequential. Between each PassPriority, the game state updates.
   - What's unclear: How does the player interrupt? They can't dispatch a CastSpell while a PassPriority is being processed.
   - Recommendation: Use a ref/flag `isResolveAllActive` that the resolve-all loop checks between iterations. When the player clicks a card to play, set the flag to false. The loop exits on next iteration check.

## Sources

### Primary (HIGH confidence)
- Alchemy source code (local: /Users/matt/dev/alchemy) — ActionButton.tsx, BlockAssignmentLines.tsx, usePhaseInfo.ts, buttonStyles.ts, boardSizing.ts
- Forge.rs source code (local: /Users/matt/dev/forge.rs) — all existing components, stores, types, engine types
- Engine Rust types — WaitingFor enum, ManaCostShard enum, CombatState, DamageAssignment

### Secondary (MEDIUM confidence)
- Architecture patterns — inferred from established project conventions (module-level arrays, WaitingFor-driven visibility, legalActions filtering)

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH — all libraries already in project, no new deps
- Architecture: HIGH — direct port of Alchemy patterns adapted for MTG, with full codebase inspection
- Pitfalls: HIGH — identified from actual code inspection and Alchemy/MTG differences
- Open questions: MEDIUM — ManaPayment cost inference and DamageAssignment modal need validation during implementation

**Research date:** 2026-03-09
**Valid until:** 2026-04-09 (stable — no external dependency changes expected)
