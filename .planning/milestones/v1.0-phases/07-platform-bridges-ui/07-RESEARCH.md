# Phase 7: Platform Bridges & UI - Research

**Researched:** 2026-03-07
**Domain:** React game UI, Zustand state management, Framer Motion animation, Tauri v2 desktop, PWA, Scryfall card images
**Confidence:** HIGH

## Summary

Phase 7 builds the complete game UI for Forge.rs — a React 19 application rendering an MTG game board with Arena-style visuals, plus a deck builder and platform bridges (PWA + Tauri desktop). The Alchemy reference project at `../alchemy` provides battle-tested patterns for Zustand stores, Framer Motion animations, board layout, glow rings, and touch interaction that should be directly adapted.

The engine already exposes all required state via serde-serialized types: `GameState` with `HashMap<ObjectId, GameObject>` for the object store, `WaitingFor` discriminated union (9 variants) driving UI prompts, `GameEvent` tagged union (30+ variants) driving the animation queue and game log, and `GameAction` for user input. The existing `EngineAdapter` interface (4 methods) and `WasmAdapter` provide the transport layer. The primary technical challenges are: (1) mapping engine state to visual components efficiently, (2) Scryfall image caching across platforms, (3) the animation queue coordinating Framer Motion with game state updates, and (4) Tauri IPC adapter.

**Primary recommendation:** Build PWA-first with Zustand stores (game, UI, animation), Tailwind v4 for styling, Framer Motion for animations, and `idb-keyval` for IndexedDB image caching. Add Tauri desktop wrapping last as a thin adapter layer.

<user_constraints>

## User Constraints (from CONTEXT.md)

### Locked Decisions
- Arena/Alchemy style visual direction — modern web feel, not Forge's utilitarian panel layout
- Reference implementation: Alchemy project at `../alchemy` (Zustand stores, Framer Motion, Tailwind, particle VFX, glow rings)
- Full animation infrastructure: Zustand animation store, Framer Motion layout animations, combat projectiles, screen shake, particle effects
- Both players' cards face the same direction (readable without rotation) — Arena convention
- Classic horizontal board layout: opponent's board top, yours bottom, hand at very bottom
- Permanents organized in free-form type rows (creatures row, lands row, other permanents) — not stacked by name
- Full Scryfall card images displayed on battlefield (reduced size), zoom on hover/long-press for full-size preview
- Arena-style tapped angle (~30-45 deg tilt, not full 90 deg)
- Counters as numbered badges on the card
- Attachments fan out slightly behind the permanent
- Damage shown as red overlay on toughness
- Glow ring for summoning sickness (desaturated like Arena)
- Glow rings color-coded: cyan=target, white=interactable
- Pre-download all card images when a deck is loaded/selected (batch fetch before game starts)
- Cache to IndexedDB (PWA) or filesystem (Tauri)
- Auto-tap with manual override for mana payment
- Valid targets glow with colored ring, click to select, arrow drawn source-to-target
- Auto-target when exactly one legal target
- Auto-pass priority when no legal plays; manual pass button; Full Control toggle; keyboard shortcut
- Modal choices: overlay card large in center with clickable options
- Replacement effect ordering: show competing effects as cards with "choose which applies first"
- Deck builder: visual grid + sidebar layout with search/filter, Scryfall image grid, deck list, mana curve chart
- Standard format legality filtering
- Import .dck/.dec files from Forge
- PWA-first development, Tauri desktop wrapping at the end
- TauriAdapter implements existing EngineAdapter interface
- Responsive breakpoints for tablet/touch
- Long-press for card inspection on touch devices

### Claude's Discretion
- Scryfall image size selection (normal vs small vs large)
- Exact Zustand store structure (game, animation, UI stores)
- Framer Motion animation timing and easing
- Particle effect detail level
- CSS/Tailwind approach (Tailwind v4 like Alchemy, or other)
- Card sizing responsive breakpoints
- Game log format and detail level
- Undo UX for unrevealed-information actions (QOL-01)
- Keyboard shortcut bindings (QOL-02)
- Card coverage dashboard layout (QOL-03)
- Stack visualization design
- Phase tracker visual style

### Deferred Ideas (OUT OF SCOPE)
None — discussion stayed within phase scope

</user_constraints>

<phase_requirements>

## Phase Requirements

| ID | Description | Research Support |
|----|-------------|-----------------|
| UI-01 | Battlefield layout with permanents, tap state, attachments, counters | Alchemy GameBoard/CreatureSlots patterns, GameObject fields (tapped, counters, attachments, entered_battlefield_turn for summoning sickness) |
| UI-02 | Hand display with legal-play highlighting | Alchemy PlayerHand pattern, WaitingFor::Priority + legal action enumeration |
| UI-03 | Stack visualization | StackEntry with StackEntryKind discriminated union, vertical card list |
| UI-04 | Phase/turn tracker | Phase enum (12 variants), turn_number, active_player from GameState |
| UI-05 | Life total display | Player.life from GameState, LifeChanged events for animation |
| UI-06 | Targeting UI with valid target highlighting | WaitingFor::TargetSelection, glow ring pattern from Alchemy |
| UI-07 | Mana payment UI with auto-tap and manual override | WaitingFor::ManaPayment, engine greedy auto-tap, manual land clicking |
| UI-08 | Card preview/zoom with Scryfall images | Scryfall API normal (488x680) for battlefield, large (672x936) for zoom |
| UI-09 | Choice prompts for modal effects | WaitingFor variants + modal overlay pattern |
| UI-10 | Game log | GameEvent stream (30+ event types), formatted text log |
| UI-11 | Touch-optimized responsive design | CSS custom properties for card sizing (Alchemy pattern), long-press |
| DECK-01 | Deck builder with card search and filtering | Scryfall search API, visual grid layout |
| DECK-02 | Import .dck/.dec files from Forge | Text parsing (card name + count per line) |
| DECK-03 | Mana curve and color distribution display | Derived from deck card data, chart component |
| PLAT-01 | Tauri desktop app | Tauri v2 with `@tauri-apps/cli`, `#[tauri::command]` for IPC |
| PLAT-02 | PWA + WASM build for tablet/browser | Existing vite-plugin-wasm + WasmAdapter, add PWA manifest + service worker |
| PLAT-04 | Scryfall card image caching | idb-keyval for IndexedDB (PWA), Tauri filesystem API (desktop) |
| QOL-01 | Undo for unrevealed-information actions | State snapshot before action, restore on undo |
| QOL-02 | Keyboard shortcuts | Event listeners for pass turn, full control toggle, tap all lands |
| QOL-03 | Card coverage dashboard | Query engine's registered card handlers, display as searchable list |

</phase_requirements>

## Standard Stack

### Core
| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| react | ^19.0.0 | UI framework | Already installed, React 19 with Suspense |
| react-dom | ^19.0.0 | DOM rendering | Already installed |
| zustand | ^5.0 | State management | Alchemy uses it; minimal API, no Provider, subscribeWithSelector middleware |
| framer-motion | ^12.35 | Animation | Alchemy uses it; layout animations, AnimatePresence, gesture support |
| react-router | ^7.0 | Routing | Menu/game/deck-builder navigation; v7 consolidates react-router-dom |
| tailwindcss | ^4.2 | CSS utility framework | Alchemy uses Tailwind v4; CSS-first config, Vite plugin |
| @tailwindcss/vite | ^4.2 | Vite integration | First-party Tailwind v4 plugin for Vite |
| idb-keyval | ^6.2 | IndexedDB key-value store | Tiny (600B), promise-based, ideal for image blob caching |

### Supporting
| Library | Version | Purpose | When to Use |
|---------|---------|---------|-------------|
| @tauri-apps/api | ^2.0 | Tauri IPC from TypeScript | TauriAdapter implementation, filesystem access |
| @tauri-apps/cli | ^2.0 | Tauri build tooling | Desktop app packaging |
| vite-plugin-pwa | ^1.2 | PWA manifest + service worker | PWA support (service worker for offline, manifest for install) |

### Alternatives Considered
| Instead of | Could Use | Tradeoff |
|------------|-----------|----------|
| zustand | Redux Toolkit | More boilerplate; Alchemy already proves zustand works for card games |
| framer-motion | react-spring | Framer Motion's layout animations and AnimatePresence are better for card repositioning |
| idb-keyval | localForage | localForage is larger; idb-keyval is sufficient for simple key-value blob storage |
| react-router | TanStack Router | react-router v7 is simpler for this use case (3-4 routes) |

**Installation:**
```bash
pnpm add zustand framer-motion react-router tailwindcss @tailwindcss/vite idb-keyval
pnpm add -D @tauri-apps/cli vite-plugin-pwa
```

## Architecture Patterns

### Recommended Project Structure
```
client/src/
├── adapter/             # EngineAdapter interface + implementations (exists)
│   ├── types.ts         # GameState, GameAction, GameEvent types (expand)
│   ├── wasm-adapter.ts  # WASM transport (exists)
│   └── tauri-adapter.ts # Tauri IPC transport (new)
├── stores/              # Zustand stores
│   ├── gameStore.ts     # Engine state, dispatch actions, legal actions
│   ├── uiStore.ts       # Selection, hover, targeting, inspection
│   └── animationStore.ts# Animation queue, position registry
├── components/
│   ├── board/           # GameBoard, BattlefieldRow, PermanentCard
│   ├── hand/            # PlayerHand, OpponentHand
│   ├── stack/           # StackDisplay, StackEntry
│   ├── controls/        # PhaseTracker, LifeTotal, PassButton, FullControlToggle
│   ├── targeting/       # TargetingOverlay, TargetArrow
│   ├── animation/       # AnimationOverlay, ParticleCanvas, FloatingNumber
│   ├── card/            # CardImage, CardPreview, CardZoom
│   ├── mana/            # ManaPaymentUI, ManaBadge
│   ├── log/             # GameLog
│   ├── modal/           # ChoiceModal, ReplacementModal
│   └── deck-builder/    # DeckBuilder, CardGrid, DeckList, ManaCurve
├── hooks/               # Custom hooks
│   ├── useGameDispatch.ts
│   ├── useCardImage.ts  # Scryfall image loading with cache
│   ├── useKeyboardShortcuts.ts
│   └── useLongPress.ts
├── services/
│   ├── scryfall.ts      # Scryfall API client
│   └── imageCache.ts    # IndexedDB/filesystem image caching
├── pages/
│   ├── MenuPage.tsx
│   ├── GamePage.tsx
│   └── DeckBuilderPage.tsx
├── wasm/                # WASM bindings (exists)
├── App.tsx              # Router shell
└── main.tsx             # Entry point
```

### Pattern 1: Zustand Store Architecture (3-store split)
**What:** Separate game state, UI state, and animation state into three Zustand stores
**When to use:** Always — matches Alchemy's proven pattern, prevents unnecessary re-renders
**Example:**
```typescript
// Source: Alchemy gameStore.ts pattern adapted for engine adapter
import { create } from 'zustand';
import { subscribeWithSelector } from 'zustand/middleware';
import type { EngineAdapter, GameState, GameAction, GameEvent } from '../adapter/types';

interface GameStore {
  state: GameState | null;
  events: GameEvent[];
  adapter: EngineAdapter | null;
  waitingFor: WaitingFor | null;

  initGame: (adapter: EngineAdapter) => Promise<void>;
  dispatch: (action: GameAction) => Promise<GameEvent[]>;
  reset: () => void;
}

export const useGameStore = create<GameStore>()(
  subscribeWithSelector((set, get) => ({
    state: null,
    events: [],
    adapter: null,
    waitingFor: null,

    initGame: async (adapter) => {
      await adapter.initialize();
      const state = await adapter.getState();
      set({ adapter, state, waitingFor: state.waiting_for });
    },

    dispatch: async (action) => {
      const { adapter } = get();
      if (!adapter) throw new Error('Not initialized');
      const events = await adapter.submitAction(action);
      const state = await adapter.getState();
      set({ state, events, waitingFor: state.waiting_for });
      return events;
    },

    reset: () => {
      const { adapter } = get();
      adapter?.dispose();
      set({ state: null, events: [], adapter: null, waitingFor: null });
    },
  }))
);
```

### Pattern 2: Animation Queue (event-driven)
**What:** GameEvents from engine dispatch feed an animation queue that plays sequentially before state renders
**When to use:** Every game action that produces visual effects
**Example:**
```typescript
// Source: Alchemy animationStore.ts / dispatchWithAnimations.ts pattern
// 1. Player dispatches action
// 2. Engine returns events
// 3. Events mapped to AnimationEffects (damage numbers, zone transitions, combat strikes)
// 4. Animation store plays effects sequentially with Framer Motion
// 5. Final state renders when animation queue drains
```

### Pattern 3: WaitingFor-driven UI Prompts
**What:** The engine's `WaitingFor` discriminated union drives which UI prompt/overlay is shown
**When to use:** All interactive game moments
**Example:**
```typescript
// WaitingFor variants -> UI components
// Priority          -> show pass button, enable legal plays in hand
// TargetSelection   -> show targeting overlay with glow rings
// ManaPayment       -> show mana payment panel
// DeclareAttackers  -> enable creature selection for attacks
// DeclareBlockers   -> enable blocker assignment
// MulliganDecision  -> show keep/mulligan buttons
// ReplacementChoice -> show replacement effect modal
// GameOver          -> show game over screen
```

### Pattern 4: Scryfall Image Loading with Cache
**What:** Pre-fetch all deck card images on game start, cache in IndexedDB, serve from cache thereafter
**When to use:** Deck loading and in-game card display
**Example:**
```typescript
// 1. Deck loaded -> extract unique card names
// 2. For each card, check IndexedDB cache (idb-keyval)
// 3. Cache miss: fetch from Scryfall (50-100ms delay between requests)
// 4. Store blob in IndexedDB with card name as key
// 5. CardImage component reads from cache, shows placeholder frame while loading
```

### Pattern 5: CSS Custom Properties for Responsive Card Sizing
**What:** Card dimensions defined as CSS custom properties, adjusted via media queries
**When to use:** All card rendering — battlefield, hand, deck builder
**Example (from Alchemy):**
```css
:root {
  --card-w: 90px;
  --card-h: 130px;
}
@media (min-width: 768px) and (min-height: 500px) {
  :root { --card-w: 115px; --card-h: 165px; }
}
@media (min-width: 1200px) {
  :root { --card-w: 140px; --card-h: 200px; }
}
```

### Anti-Patterns to Avoid
- **Polling engine state:** Use event-driven updates from `submitAction` return, not periodic `getState` calls
- **Single monolithic store:** Split into game/UI/animation stores to prevent re-render cascading
- **Manual Scryfall URL construction:** Always use API to get image URIs — Scryfall changes CDN paths
- **Synchronous WASM calls in render:** All WASM calls go through async adapter queue
- **90-degree tap rotation:** Arena uses ~30-45 degrees to keep card art visible

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| IndexedDB access | Raw IndexedDB API | `idb-keyval` | IndexedDB API is callback-hell; idb-keyval is 600B and promise-based |
| Layout animations | CSS transitions on card position | Framer Motion `layoutId` + `AnimatePresence` | FLIP animations are extremely tricky to get right manually |
| Service worker | Manual service worker registration | `vite-plugin-pwa` + Workbox | SW lifecycle, cache strategies, update detection are error-prone |
| Desktop packaging | Electron or manual native wrapper | Tauri v2 | Tauri gives tiny binary, native perf, same webview |
| Mana curve chart | D3.js or Chart.js | Simple div bars with Tailwind | Mana curve is just 7 vertical bars — a full charting library is overkill |
| Card image fetching | Manual fetch + blob storage | Scryfall API + idb-keyval | Rate limiting, URL management, cache invalidation |
| Keyboard shortcuts | Raw addEventListener | Custom hook with cleanup | Need proper cleanup on unmount, conflict detection |

**Key insight:** The Alchemy project has already solved most UI patterns needed here. Adapt its store architecture, animation pipeline, and component patterns rather than inventing new approaches.

## Common Pitfalls

### Pitfall 1: Scryfall Rate Limiting
**What goes wrong:** App fetches card images too aggressively, gets 429'd or IP-banned
**Why it happens:** Loading a 60-card deck means 60 API calls if not cached
**How to avoid:** Batch pre-fetch with 50-100ms delays between requests; cache aggressively in IndexedDB; use bulk data for card metadata (not images)
**Warning signs:** 429 responses, slow image loading, blank cards

### Pitfall 2: WASM Thread Blocking
**What goes wrong:** UI freezes during engine computation
**Why it happens:** WASM runs on the main thread in browsers
**How to avoid:** WasmAdapter already uses async queue serialization; ensure long operations yield to event loop; consider Web Worker for AI turns (Phase 8)
**Warning signs:** UI becomes unresponsive during stack resolution or SBA loops

### Pitfall 3: Animation-State Desync
**What goes wrong:** Visual state doesn't match engine state; cards appear in wrong zones
**Why it happens:** Rendering new state before animation completes, or animation referencing stale positions
**How to avoid:** Use Alchemy's dispatchWithAnimations pattern — snapshot positions before dispatch, queue animations, render final state only when queue drains
**Warning signs:** Cards jumping positions, phantom cards, counters not updating

### Pitfall 4: Zustand Re-render Storms
**What goes wrong:** Entire board re-renders on every state change
**Why it happens:** Subscribing to entire GameState instead of selecting slices
**How to avoid:** Use Zustand selectors with `subscribeWithSelector` middleware; select only the specific fields each component needs; use `shallow` equality for arrays
**Warning signs:** Laggy UI, React DevTools showing excessive re-renders

### Pitfall 5: Double-Faced Card Image Handling
**What goes wrong:** Transform/MDFC cards show wrong face or no image
**Why it happens:** Scryfall stores these in `card_faces[].image_uris` instead of top-level `image_uris`
**How to avoid:** Check for `card_faces` array; use `transformed` field on GameObject to select correct face
**Warning signs:** Blank images on DFCs, wrong art showing after transform

### Pitfall 6: Touch vs Mouse Interaction Conflicts
**What goes wrong:** Hover effects fire on touch, long-press conflicts with scroll, tap targets too small
**Why it happens:** Mouse and touch events have different semantics
**How to avoid:** Use `@media (hover: hover)` for hover effects; implement long-press via custom hook with touch cancel on scroll; minimum 44px tap targets
**Warning signs:** Accidental card inspections while scrolling, unreachable buttons on mobile

### Pitfall 7: Tauri + WASM Dual Build
**What goes wrong:** Same WASM engine loaded in both webview and native process
**Why it happens:** Tauri runs Rust natively — no need for WASM in desktop mode
**How to avoid:** TauriAdapter uses IPC to native Rust engine (not WASM); WasmAdapter used only in PWA mode; detect platform at runtime to select adapter
**Warning signs:** Double memory usage, native engine and WASM engine out of sync

## Code Examples

### Scryfall Image Fetching with Cache
```typescript
// Source: Scryfall API docs + idb-keyval patterns
import { get, set } from 'idb-keyval';

const SCRYFALL_DELAY_MS = 75;

interface ScryfallCard {
  image_uris?: { normal: string; large: string; small: string };
  card_faces?: Array<{ image_uris?: { normal: string; large: string; small: string } }>;
}

async function fetchCardImage(cardName: string, size: 'normal' | 'large' = 'normal'): Promise<Blob> {
  const cacheKey = `scryfall:${cardName}:${size}`;
  const cached = await get<Blob>(cacheKey);
  if (cached) return cached;

  const resp = await fetch(
    `https://api.scryfall.com/cards/named?exact=${encodeURIComponent(cardName)}`
  );
  const card: ScryfallCard = await resp.json();

  const imageUrl = card.image_uris?.[size]
    ?? card.card_faces?.[0]?.image_uris?.[size];
  if (!imageUrl) throw new Error(`No image for ${cardName}`);

  const imgResp = await fetch(imageUrl);
  const blob = await imgResp.blob();
  await set(cacheKey, blob);
  return blob;
}

async function prefetchDeckImages(cardNames: string[]): Promise<void> {
  const unique = [...new Set(cardNames)];
  for (const name of unique) {
    await fetchCardImage(name, 'normal');
    await new Promise(r => setTimeout(r, SCRYFALL_DELAY_MS));
  }
}
```

### TauriAdapter Implementation
```typescript
// Source: Tauri v2 docs — Calling Rust from the Frontend
import { invoke } from '@tauri-apps/api/core';
import type { EngineAdapter, GameState, GameAction, GameEvent } from './types';

export class TauriAdapter implements EngineAdapter {
  async initialize(): Promise<void> {
    await invoke('initialize_game');
  }

  async submitAction(action: GameAction): Promise<GameEvent[]> {
    return invoke('submit_action', { action });
  }

  async getState(): Promise<GameState> {
    return invoke('get_game_state');
  }

  dispose(): void {
    invoke('dispose_game').catch(() => {});
  }
}
```

### Glow Ring Pattern (Alchemy-style)
```typescript
// Source: Alchemy component patterns
// Glow rings via box-shadow or ring utility with color variants
// - cyan (#00e5ff) ring: valid target
// - white ring: interactable / can be played
// - desaturated ring: summoning sickness
// - pulsing animation on hover

// CSS (Tailwind v4):
// .card-interactable { box-shadow: 0 0 8px 2px rgba(255,255,255,0.6); }
// .card-target       { box-shadow: 0 0 12px 3px rgba(0,229,255,0.8); }
// .card-sick         { filter: saturate(0.5); box-shadow: 0 0 6px 1px rgba(255,255,255,0.3); }
```

### Deck File Parser (.dck/.dec)
```typescript
// Forge .dck format: "count CardName" per line
// Section headers: [Main] [Sideboard]
function parseDeckFile(content: string): { main: DeckEntry[]; sideboard: DeckEntry[] } {
  const lines = content.split('\n');
  let section: 'main' | 'sideboard' = 'main';
  const main: DeckEntry[] = [];
  const sideboard: DeckEntry[] = [];

  for (const line of lines) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith('#')) continue;
    if (trimmed === '[Main]' || trimmed === '[main]') { section = 'main'; continue; }
    if (trimmed === '[Sideboard]' || trimmed === '[sideboard]') { section = 'sideboard'; continue; }

    const match = trimmed.match(/^(\d+)\s+(.+)$/);
    if (match) {
      const entry = { count: parseInt(match[1]), name: match[2] };
      (section === 'main' ? main : sideboard).push(entry);
    }
  }
  return { main, sideboard };
}
```

## Scryfall Image Size Recommendation

| Size | Dimensions | Format | File Size (~) | Use Case |
|------|-----------|--------|---------------|----------|
| small | 146x204 | JPG | ~10KB | Deck builder grid thumbnails |
| normal | 488x680 | JPG | ~60KB | Battlefield cards (reduced display) |
| large | 672x936 | JPG | ~100KB | Hover/long-press zoom preview |

**Recommendation:** Cache `normal` for all cards on deck load (primary display). Fetch `large` on-demand for zoom preview. Use `small` in deck builder search results to reduce bandwidth. A 60-card deck with unique cards: ~60 * 60KB = ~3.6MB for normal images — very manageable for IndexedDB.

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| Tailwind v3 config.js | Tailwind v4 CSS-first `@import "tailwindcss"` | Jan 2025 | No JS config file, auto template discovery |
| framer-motion package | motion package (v12) | 2024 | Rebranded but `framer-motion` import still works, no breaking changes |
| react-router-dom | react-router v7 | 2024 | Package consolidated, same API |
| Tauri v1 | Tauri v2 | 2024 | New plugin system, mobile support, improved IPC |
| Zustand v4 | Zustand v5 | 2024 | Cleaner TypeScript, same core API |

**Deprecated/outdated:**
- Tailwind `tailwind.config.js` — v4 uses CSS-only config via `@import "tailwindcss"`
- `react-router-dom` separate package — v7 consolidates into `react-router`
- Manual service worker — use `vite-plugin-pwa` with Workbox

## Open Questions

1. **Engine WASM API expansion**
   - What we know: Current WASM bindings expose `create_initial_state`, `ping`, and basic types
   - What's unclear: Full `submitAction`/`getState` may need additional wasm-bindgen exports for complete game interaction
   - Recommendation: Verify and expand WASM bindings early in implementation; the engine types (GameState, GameEvent, WaitingFor) need tsify exports

2. **Undo implementation for unrevealed-information actions (QOL-01)**
   - What we know: Engine state is serializable via serde
   - What's unclear: Whether to snapshot full GameState or use engine-level undo support
   - Recommendation: Serialize GameState before each action; restore on undo if action involved only unrevealed information (no cards drawn, no random choices)

3. **Full Control toggle interaction with auto-pass**
   - What we know: Arena has this exact feature
   - What's unclear: Granularity — does Full Control stop at every sub-step or just every priority pass?
   - Recommendation: Full Control = never auto-pass priority, even when no legal actions (forces explicit pass at every opportunity)

## Validation Architecture

### Test Framework
| Property | Value |
|----------|-------|
| Framework | Vitest 3.x (already configured) |
| Config file | `client/vite.config.ts` (vitest reads from Vite config) |
| Quick run command | `cd client && pnpm test -- --run` |
| Full suite command | `cd client && pnpm test -- --run` |

### Phase Requirements -> Test Map
| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| UI-01 | Battlefield renders permanents with tap state, counters, attachments | unit + integration | `cd client && pnpm test -- --run src/components/board/` | No — Wave 0 |
| UI-02 | Hand displays cards with legal-play highlighting | unit | `cd client && pnpm test -- --run src/components/hand/` | No — Wave 0 |
| UI-03 | Stack shows entries in LIFO order | unit | `cd client && pnpm test -- --run src/components/stack/` | No — Wave 0 |
| UI-04 | Phase tracker shows current phase and turn | unit | `cd client && pnpm test -- --run src/components/controls/` | No — Wave 0 |
| UI-05 | Life totals display and update on events | unit | `cd client && pnpm test -- --run src/components/controls/` | No — Wave 0 |
| UI-06 | Targeting UI shows glow rings on valid targets | unit | `cd client && pnpm test -- --run src/components/targeting/` | No — Wave 0 |
| UI-07 | Mana payment shows auto-tap with manual override | unit | `cd client && pnpm test -- --run src/components/mana/` | No — Wave 0 |
| UI-08 | Card zoom shows Scryfall image on hover/long-press | unit | `cd client && pnpm test -- --run src/components/card/` | No — Wave 0 |
| UI-09 | Choice modal displays options for modal effects | unit | `cd client && pnpm test -- --run src/components/modal/` | No — Wave 0 |
| UI-10 | Game log shows formatted events | unit | `cd client && pnpm test -- --run src/components/log/` | No — Wave 0 |
| UI-11 | Responsive layout adjusts card sizes for tablet | unit | `cd client && pnpm test -- --run src/components/board/` | No — Wave 0 |
| DECK-01 | Deck builder searches and filters cards | unit | `cd client && pnpm test -- --run src/components/deck-builder/` | No — Wave 0 |
| DECK-02 | Import .dck/.dec files parses correctly | unit | `cd client && pnpm test -- --run src/services/` | No — Wave 0 |
| DECK-03 | Mana curve chart renders from deck data | unit | `cd client && pnpm test -- --run src/components/deck-builder/` | No — Wave 0 |
| PLAT-01 | TauriAdapter implements EngineAdapter | unit | `cd client && pnpm test -- --run src/adapter/` | Partial (wasm-adapter.test.ts exists) |
| PLAT-02 | PWA + WASM build works in browser | smoke | Manual — `pnpm build && pnpm preview` | No |
| PLAT-04 | Image caching stores/retrieves from IndexedDB | unit | `cd client && pnpm test -- --run src/services/` | No — Wave 0 |
| QOL-01 | Undo restores state for unrevealed actions | unit | `cd client && pnpm test -- --run src/stores/` | No — Wave 0 |
| QOL-02 | Keyboard shortcuts trigger correct actions | unit | `cd client && pnpm test -- --run src/hooks/` | No — Wave 0 |
| QOL-03 | Card coverage dashboard renders | unit | `cd client && pnpm test -- --run src/components/` | No — Wave 0 |

### Sampling Rate
- **Per task commit:** `cd client && pnpm test -- --run`
- **Per wave merge:** `cd client && pnpm test -- --run`
- **Phase gate:** Full suite green before `/gsd:verify-work`

### Wave 0 Gaps
- [ ] `client/src/stores/__tests__/gameStore.test.ts` — covers game state management
- [ ] `client/src/stores/__tests__/uiStore.test.ts` — covers UI state
- [ ] `client/src/services/__tests__/imageCache.test.ts` — covers PLAT-04
- [ ] `client/src/services/__tests__/deckParser.test.ts` — covers DECK-02
- [ ] Zustand + Framer Motion test utilities / shared fixtures
- [ ] Note: Testing Library React already installed as dev dependency

## Sources

### Primary (HIGH confidence)
- Alchemy project at `../alchemy` — game store pattern, UI store, animation store, board components, card rendering, Tailwind v4 setup
- Forge.rs engine types at `crates/engine/src/types/` — GameState, GameObject, WaitingFor, GameEvent, GameAction structures
- Existing client at `client/src/` — EngineAdapter interface, WasmAdapter, WASM bindings, Vite config
- [Scryfall Card Imagery docs](https://scryfall.com/docs/api/images) — image sizes and formats
- [Scryfall API docs](https://scryfall.com/docs/api) — rate limits (50-100ms delay, 10 req/sec)
- [Tauri v2 docs](https://v2.tauri.app/develop/calling-rust/) — IPC command system

### Secondary (MEDIUM confidence)
- [Tailwind v4 announcement](https://tailwindcss.com/blog/tailwindcss-v4) — CSS-first config, Vite plugin
- [Motion docs](https://motion.dev/docs/react) — Framer Motion v12 API (no breaking changes from v11)
- [Zustand GitHub](https://github.com/pmndrs/zustand) — v5 API, subscribeWithSelector middleware
- [React Router v7](https://reactrouter.com/) — package consolidation, React 19 support

### Tertiary (LOW confidence)
- Scryfall image CDN changes — [blog post about upcoming URI changes](https://scryfall.com/blog/upcoming-api-changes-to-scryfall-image-uris-and-download-uris-224) — may affect direct URL caching strategy

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH — Alchemy project proves entire stack works for card game UI; all libraries current
- Architecture: HIGH — Direct adaptation of Alchemy patterns with engine adapter abstraction
- Pitfalls: HIGH — Known from Alchemy development experience and Scryfall API documentation
- Scryfall integration: MEDIUM — API is stable but CDN URL patterns may change; always use API responses for URLs
- Tauri integration: MEDIUM — Standard Tauri v2 patterns, but TauriAdapter hasn't been built yet

**Research date:** 2026-03-07
**Valid until:** 2026-04-07 (stable ecosystem, 30-day validity)
