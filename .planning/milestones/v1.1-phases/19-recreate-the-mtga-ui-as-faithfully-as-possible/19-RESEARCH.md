# Phase 19: Recreate the MTGA UI as faithfully as possible - Research

**Researched:** 2026-03-09
**Domain:** React UI overhaul — card presentation, board layout, animations, menu/lobby, deck builder
**Confidence:** HIGH

## Summary

Phase 19 is a large visual/interaction fidelity overhaul with zero engine changes. The current codebase already has all the infrastructure needed: Framer Motion for animations, a Canvas particle system, Zustand stores, Scryfall image fetching with IndexedDB caching, CSS custom properties for card sizing, and a component hierarchy that maps cleanly to the changes. The key challenges are: (1) switching battlefield cards from full Scryfall `small` images to `art_crop` images with color-coded borders and overlays, (2) restructuring the GamePage layout from a stacked flow layout to an MTGA-faithful zone arrangement with centered avatars, (3) implementing several new animation effects (death shatter, cast arc, golden targeting arcs, cinematic turn banner), and (4) overhauling the MenuPage into a mode-first flow with splash screen and deck gallery.

No new dependencies are required. The existing stack (React 19, Framer Motion 12, Tailwind v4, Zustand 5, Canvas 2D, idb-keyval) is sufficient for everything described. The Scryfall `art_crop` API variant is already supported by the existing `fetchCardImageUrl` and `useCardImage` infrastructure — it just needs the size type expanded from `"small" | "normal" | "large"` to include `"art_crop"`.

**Primary recommendation:** Break this phase into 8-10 plans organized by concern: image infrastructure, card presentation, board layout, HUD/avatar repositioning, combat/targeting arcs, animation effects (turn banner, death shatter, cast arc), menu/lobby overhaul, mulligan/game-over screens, and deck builder polish.

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions
- Art crop images on battlefield (Scryfall `art_crop` variant, ~40KB each)
- No mana cost pips on battlefield cards
- Color-coded border by card color identity: White=cream, Blue=blue, Black=dark, Red=red, Green=green, Multicolor=gold, Colorless=gray
- Small card name label always visible below the art crop
- Same art-crop treatment for all permanent types
- Creatures: P/T box overlay (existing color-coding)
- Planeswalkers: loyalty shield badge
- Tokens: same art crop style, thinner frame to distinguish
- Counter badges: circular overlay at top-right with count number
- Full Scryfall card images (`normal` variant) in player's hand
- Full card images on the stack (no change from Phase 17)
- Opponent hand: classic MTG card back images fanned at top
- Dual Scryfall image fetching: `art_crop` for battlefield, `normal` for hand/stack/preview
- Player avatar: CENTER, directly above the player hand
- Opponent avatar: top-center, BELOW their hand
- Phase indicators flanking the player avatar (left: Upkeep/Draw/Main1, right: Main2/End)
- Combat sub-phases: near the action button group on the right side
- Zone order: creatures NEAR center, lands FAR from center (both sides)
- Attackers slide forward toward center during declare attackers
- Bottom-left: player graveyard + library
- Battlefield background images (NO gradients, NO vignette, NO post-processing)
- No visible zone lane borders — invisible spatial grouping only
- No bars anywhere — everything floats
- Tapped permanents: 15-20 degree clockwise rotation (NOT 90 degrees) with slight opacity dim (~0.85)
- Legal attackers/blockers: cyan glow (existing, don't change)
- Selected attackers/blockers: orange glow
- Golden targeting arcs (curved lines) connecting spells/attackers to targets
- Valid targets get golden glow during target selection
- Card play: card flies from hand to stack with arc trajectory, glow on arrival
- Stack resolve: card flies from stack to battlefield (permanents) or fades (instants/sorceries)
- Creature death: card flashes red, shatters into 8-12 art fragments, fragments scatter + fade (0.6s), particle burst at death position
- Damage impact: red particle burst on target + floating damage number + screen shake on lethal
- Turn banner: port Alchemy's TurnBanner (layered cinematic with amber/slate theming, 1.5s duration)
- Coin flip / play/draw reveal animation at game start
- Card hover on battlefield: instant full-card preview panel on right side (NO slide animation)
- Mulligan: full-screen MTGA-style with large full-card images centered
- Game over: dramatic full-screen "VICTORY"/"DEFEAT" overlays
- Mode selection first (Play vs AI, Play Online, Deck Builder)
- After choosing mode, deck gallery screen with card art tiles
- AI difficulty: inline segmented control in deck gallery (Easy/Medium/Hard)
- Last-used deck remembered and pre-selected
- Animated particle background on menu
- Brief splash screen with Forge.rs logo + loading bar during WASM init
- Logo: convert existing PNG to WebP at quality 85
- Deck builder: art-crop image grid with card count overlays
- Deck builder: visual color/type filtering
- Deck builder: same instant preview panel as game board

### Claude's Discretion
- Exact art-crop card dimensions and aspect ratio on battlefield
- Color-coded border exact hex values and width
- Counter badge exact positioning and sizing
- Attacker slide distance and animation timing
- Targeting arc curve parameters and animation
- Death shatter fragment count, scatter pattern, and timing
- Particle background implementation details for menu
- Splash screen animation timing and loading bar style
- Deck gallery tile sizing and grid layout
- Deck builder filter UI design
- How to pick "representative card" art for deck gallery tiles
- Graveyard and library visual design at bottom-left
- Whether center divider line exists at all or is purely invisible

### Deferred Ideas (OUT OF SCOPE)
- Combat math bubbles
- Manual mana tapping mode
- Stack item hover preview
- Deck builder advanced features (import/export improvements, sideboard management)
- Animated card art on battlefield (like MTGA's premium card animations)
- Deckbuilder MTGA-style collection view with 3D card flipping
</user_constraints>

## Standard Stack

### Core
| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| React | 19.0.0 | Component framework | Already in use |
| Framer Motion | 12.35.1 | All animations (layout, enter/exit, gestures) | Already in use, handles layout animations, AnimatePresence |
| Tailwind CSS | 4.2.1 | Styling | Already in use |
| Zustand | 5.0.11 | State management | Already in use for gameStore, uiStore, animationStore |
| idb-keyval | 6.2.2 | IndexedDB caching for Scryfall images | Already in use |
| Canvas 2D API | Native | Particle effects, death shatter fragments | Already used by ParticleCanvas |
| SVG | Native | Targeting arcs, block assignment lines | Already used by TargetArrow, BlockAssignmentLines |

### No New Dependencies Needed

All required functionality is achievable with the existing stack. Canvas 2D handles the death shatter effect (draw card art fragments, animate scatter). SVG quadratic Bezier curves handle golden targeting arcs. Framer Motion handles all UI transitions, layout animations, and the cast/resolve card flight paths.

## Architecture Patterns

### Recommended Change Organization
```
client/src/
├── services/
│   └── scryfall.ts              # Extend ImageSize type to include "art_crop"
├── hooks/
│   └── useCardImage.ts          # Works as-is (already accepts size param)
├── components/
│   ├── card/
│   │   ├── CardImage.tsx         # Current full-card renderer (hand/stack use)
│   │   ├── ArtCropCard.tsx       # NEW: art-crop battlefield renderer
│   │   └── CardPreview.tsx       # Make instant (remove animation transition)
│   ├── board/
│   │   ├── PermanentCard.tsx     # Switch to ArtCropCard, change tap rotation
│   │   ├── GameBoard.tsx         # Reverse zone order (creatures near center)
│   │   ├── BattlefieldRow.tsx    # Remove border styling, invisible grouping
│   │   ├── BlockAssignmentLines  # Extend for golden targeting arcs
│   │   └── ActionButton.tsx      # Relocate, add combat phase indicators
│   ├── hud/
│   │   ├── PlayerHud.tsx         # Redesign as centered avatar
│   │   └── OpponentHud.tsx       # Redesign as centered avatar
│   ├── animation/
│   │   ├── TurnBanner.tsx        # Replace with cinematic version
│   │   ├── DeathShatter.tsx      # NEW: canvas-based shatter effect
│   │   ├── CastArcAnimation.tsx  # NEW: card flight from hand to stack
│   │   ├── ResolveAnimation.tsx  # NEW: card flight from stack to battlefield
│   │   ├── AnimationOverlay.tsx  # Wire new animation types
│   │   └── ParticleCanvas.tsx    # Extend for menu background particles
│   ├── targeting/
│   │   ├── TargetArrow.tsx       # Change to golden curved arc
│   │   └── TargetingOverlay.tsx  # Golden glow on valid targets
│   ├── controls/
│   │   ├── PhaseStopBar.tsx      # Relocate to flank avatar
│   │   └── PhaseIndicator.tsx    # NEW: split into combat/non-combat groups
│   ├── zone/
│   │   └── ZoneIndicator.tsx     # Relocate to bottom-left
│   ├── splash/
│   │   └── SplashScreen.tsx      # NEW: logo + loading bar
│   └── deck-builder/
│       ├── CardGrid.tsx          # Switch to art_crop images
│       └── DeckGallery.tsx       # NEW: deck selection with art tiles
├── pages/
│   ├── MenuPage.tsx              # Complete overhaul: mode-first flow
│   ├── GamePage.tsx              # Layout restructure + new overlays
│   └── DeckBuilderPage.tsx       # Add preview panel + filter bar
└── index.css                     # Add art-crop card CSS variables
```

### Pattern 1: Art-Crop Card Component
**What:** New `ArtCropCard` component that renders Scryfall `art_crop` images with color-coded borders, card name label, and overlay elements (P/T, loyalty, counters).
**When to use:** All battlefield permanents.
**Example:**
```typescript
// ArtCropCard.tsx
interface ArtCropCardProps {
  objectId: number;
}

// Color border mapping
const BORDER_COLORS: Record<string, string> = {
  White: '#F5E6C8',   // cream
  Blue: '#0E68AB',    // blue
  Black: '#2B2B2B',   // dark
  Red: '#D32029',     // red
  Green: '#00733E',   // green
  Multi: '#C9B037',   // gold (2+ colors)
  Colorless: '#8E8E8E', // gray
};

function getBorderColor(colors: string[]): string {
  if (colors.length === 0) return BORDER_COLORS.Colorless;
  if (colors.length > 1) return BORDER_COLORS.Multi;
  return BORDER_COLORS[colors[0]] ?? BORDER_COLORS.Colorless;
}

// Renders: art_crop image + color border + name label + P/T or loyalty overlay
```

### Pattern 2: Tap Rotation Override
**What:** Change tap rotation from 90 degrees to ~17 degrees with opacity dim.
**Where:** `PermanentCard.tsx` — the `animate` prop on the `motion.div`.
```typescript
// Before (Phase 13):
animate={{ rotate: isAttacking || obj.tapped ? 90 : 0 }}

// After (Phase 19):
animate={{
  rotate: isAttacking || obj.tapped ? 17 : 0,
  opacity: obj.tapped ? 0.85 : 1,
}}
```

### Pattern 3: Death Shatter via Canvas
**What:** When a creature dies, capture its art-crop as a canvas image, split into 8-12 Voronoi-like fragments, animate scatter/fade.
**Implementation:** Use existing `ParticleCanvas` infrastructure. On death event, draw the card art onto an offscreen canvas, slice into triangular/polygon fragments using canvas clipping, then animate each fragment with velocity + gravity + fade.
```typescript
// In AnimationOverlay processEffect for CreatureDestroyed:
// 1. Get card art image from cache/DOM
// 2. Create offscreen canvas, draw image
// 3. Generate 8-12 random polygon clips
// 4. For each fragment: spawn as particle with position, velocity, rotation, alpha decay
// 5. ParticleCanvas renders fragments as textured particles
```

### Pattern 4: SVG Quadratic Bezier Targeting Arcs
**What:** Replace straight targeting lines with golden curved arcs.
**Implementation:** Use SVG `<path>` with quadratic Bezier `Q` command. Control point offset perpendicular to the midpoint of source-to-target line.
```typescript
// Golden arc path
function getArcPath(from: Point, to: Point): string {
  const mx = (from.x + to.x) / 2;
  const my = (from.y + to.y) / 2;
  const dx = to.x - from.x;
  const dy = to.y - from.y;
  // Perpendicular offset for curve
  const offset = Math.min(80, Math.sqrt(dx * dx + dy * dy) * 0.3);
  const cx = mx - (dy / Math.sqrt(dx * dx + dy * dy)) * offset;
  const cy = my + (dx / Math.sqrt(dx * dx + dy * dy)) * offset;
  return `M ${from.x} ${from.y} Q ${cx} ${cy} ${to.x} ${to.y}`;
}
// Stroke: gold (#C9B037) with glow filter, animated pathLength
```

### Pattern 5: Board Layout Restructure
**What:** GamePage layout changes from top-to-bottom stacked to MTGA-faithful arrangement.
**Current layout (top to bottom):** OpponentHud, OpponentHand, Opponent Battlefield, Center Divider, Player Battlefield, PlayerHud, PlayerHand
**Target layout (top to bottom):** OpponentHand, OpponentAvatar, Opponent Lands, Opponent Creatures, [center], Player Creatures, Player Lands, PlayerAvatar + PhaseIndicators, PlayerHand
```
┌──────────────────────────────────────────┐
│         Opponent Hand (card backs)        │
│           Opponent Avatar (center)        │
│         Opponent Lands (far from center)  │
│       Opponent Creatures (near center)    │
│  ─ ─ ─ ─ ─ ─ ─ center ─ ─ ─ ─ ─ ─ ─   │
│       Player Creatures (near center)      │
│         Player Lands (far from center)    │
│    [Phase L] Player Avatar [Phase R]      │
│         Player Hand (full cards)          │
│                                           │
│  [GY+Lib bottom-left]  [Actions right]   │
│                    [Stack right-center]   │
└──────────────────────────────────────────┘
```

### Anti-Patterns to Avoid
- **Don't create separate animation libraries:** Use existing ParticleCanvas and Framer Motion. The death shatter, cast arc, and resolve animation are just new effect types in the existing animation pipeline.
- **Don't break the WaitingFor-driven component pattern:** All UI visibility continues to be driven by the engine's WaitingFor state. Phase 19 only changes how things look, not when they appear.
- **Don't add new state management patterns:** Continue using Zustand stores with module-level selector constants. No new stores needed.
- **Don't use `art_crop` in hand:** Hand uses `normal` variant for readable card text. Only battlefield and deck builder use `art_crop`.

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| Image caching | New cache system | Existing `imageCache.ts` + idb-keyval | Already handles blob caching with revocation |
| Card sizing | Manual pixel calculations | CSS custom properties (`--art-crop-w`, `--art-crop-h`) | Responsive across breakpoints |
| Animation sequencing | Custom animation scheduler | Existing `animationStore` step pipeline | Already handles step-based sequential processing |
| Particle effects | New particle engine | Existing `ParticleCanvas` | Already supports burst, trail, quality levels |
| Layout animations | Manual position tracking | Framer Motion `layoutId` | Already used on PermanentCard for smooth transitions |
| Image loading states | Custom loading logic | Existing `useCardImage` hook | Already handles cache-check, fetch, loading state |

## Common Pitfalls

### Pitfall 1: Scryfall Art Crop Aspect Ratio Variability
**What goes wrong:** art_crop images from Scryfall have varying aspect ratios depending on the card frame design. Using a fixed aspect ratio container causes letterboxing or cropping.
**Why it happens:** Scryfall's documentation states art_crop dimensions "vary."
**How to avoid:** Use `object-cover` on the image within a fixed container. The container defines the visual size; the image fills it. Typical art crops are roughly 4:3 landscape (~626x457px) but can vary. Use a consistent container aspect ratio (e.g., 4:3) and let `object-cover` handle the rest.
**Warning signs:** White/black bars appearing around card art, or art appearing distorted.

### Pitfall 2: Dual Image Loading Performance
**What goes wrong:** Loading both `art_crop` AND `normal` for every card doubles Scryfall API calls and increases initial load time.
**Why it happens:** Art_crop needed for battlefield, normal needed for hand/preview.
**How to avoid:** Load images lazily and only the variant needed. Battlefield cards load `art_crop` on mount. Hand cards load `normal` on mount. Preview panel loads `normal` only on hover. The existing `useCardImage` hook already supports this — just pass the correct `size` parameter. Prefetching should prioritize the variant most likely to be seen first (hand cards are `normal`).
**Warning signs:** Visible loading spinners on battlefield cards, Scryfall rate limiting (429 responses).

### Pitfall 3: Layout Shift During Board Restructure
**What goes wrong:** Moving HUDs, phase indicators, and zone indicators to new positions causes a jarring layout shift if done incrementally.
**Why it happens:** Changing the GamePage layout piecemeal means intermediate states where elements overlap or leave empty space.
**How to avoid:** Restructure the GamePage layout in a single plan that moves all positional elements together. Use `fixed` positioning for floating overlays (action buttons, stack, preview) so they don't participate in flow layout.
**Warning signs:** Components overlapping, empty white space, z-index stacking issues.

### Pitfall 4: Tap Rotation Breaking Hit Targets
**What goes wrong:** Changing from 90 degrees to 17 degrees tap rotation means the card's bounding box changes less dramatically, potentially affecting click targets and layout spacing.
**Why it happens:** At 90 degrees, a card's width becomes its height and vice versa. At 17 degrees, the card barely changes its bounding box. But the existing layout might rely on the 90-degree rotation creating more horizontal space for tapped cards.
**How to avoid:** Since the change is from 90 to 17 degrees, tapped cards will actually take LESS space, which is better for layout. Verify that click/hover targets on tapped cards still work correctly after rotation. The `data-object-id` attribute query selector approach for position tracking (used by BlockAssignmentLines) works regardless of rotation.
**Warning signs:** Tapped cards overlapping untapped ones, click targets not matching visual position.

### Pitfall 5: Canvas Z-Index and Pointer Events for Death Shatter
**What goes wrong:** The death shatter canvas fragments need to appear above the board but below modals, and must not block clicks on surviving cards.
**Why it happens:** Canvas is a single layer — you can't click "through" it to elements underneath.
**How to avoid:** Use `pointer-events: none` on the death shatter overlay (same pattern as existing ParticleCanvas). The shatter is purely visual — no interaction needed. Ensure z-index is between board (z-10) and modals (z-50).
**Warning signs:** Cards underneath shatter animation becoming unclickable.

### Pitfall 6: Instant Preview vs Animated Preview
**What goes wrong:** The user explicitly requested "NO slide animation" for card hover preview. The current CardPreview has `initial/animate/exit` Framer Motion transitions.
**Why it happens:** Current implementation uses opacity+scale animation (0.15s duration).
**How to avoid:** Remove the Framer Motion animation from CardPreview when triggered from battlefield hover. Either remove AnimatePresence entirely or set duration to 0. Keep the existing animation for non-battlefield contexts (hand hover) if desired, or simplify to instant everywhere.
**Warning signs:** Jarring slide-in effect when scanning multiple battlefield cards quickly.

## Code Examples

### Extending Scryfall ImageSize Type
```typescript
// services/scryfall.ts
type ImageSize = "small" | "normal" | "large" | "art_crop";
// The existing fetchCardImageUrl already passes `size` to Scryfall API URL.
// Scryfall accepts "art_crop" as a valid version parameter.
// URL: https://api.scryfall.com/cards/named?exact={name}&format=image&version=art_crop
```

### Art-Crop Card Rendering
```typescript
// components/card/ArtCropCard.tsx — battlefield permanent renderer
function ArtCropCard({ objectId }: { objectId: number }) {
  const obj = useGameStore((s) => s.gameState?.objects[objectId]);
  const { src, isLoading } = useCardImage(obj?.name ?? '', { size: 'art_crop' as never });

  const borderColor = getBorderColor(obj?.color ?? []);
  const ptDisplay = obj ? computePTDisplay(obj) : null;

  return (
    <div
      className="relative overflow-hidden rounded-md"
      style={{
        width: 'var(--art-crop-w)',
        height: 'var(--art-crop-h)',
        border: `2px solid ${borderColor}`,
      }}
    >
      {/* Art crop image */}
      <img src={src} className="h-full w-full object-cover" />

      {/* Card name label */}
      <div className="absolute bottom-0 left-0 right-0 bg-black/70 px-1 py-0.5 text-[9px] text-white truncate">
        {obj?.name}
      </div>

      {/* P/T box overlay for creatures */}
      {ptDisplay && <PTBox ptDisplay={ptDisplay} />}

      {/* Loyalty shield for planeswalkers */}
      {obj?.loyalty != null && <LoyaltyShield value={obj.loyalty} />}

      {/* Counter badges */}
      {counters.length > 0 && <CounterBadges counters={counters} />}
    </div>
  );
}
```

### CSS Custom Properties for Art-Crop Cards
```css
/* index.css additions */
:root {
  --art-crop-w: calc(14vw * var(--card-size-scale));
  --art-crop-h: calc(var(--art-crop-w) * 0.75); /* 4:3 aspect ratio */
}

@media (min-width: 768px) {
  :root { --art-crop-w: calc(9vw * var(--card-size-scale)); }
}

@media (min-width: 1200px) {
  :root { --art-crop-w: calc(5.5vw * var(--card-size-scale)); }
}
```

### Cinematic Turn Banner (Alchemy-style)
```typescript
// Layered cinematic turn banner
// Phase 1: light burst (radial gradient, 0-0.2s)
// Phase 2: banner strip slides in from left (0.1-0.4s)
// Phase 3: diamond accent shapes appear at edges (0.3-0.5s)
// Phase 4: text with triple glow punches in with scale (0.3-0.6s)
// Phase 5: hold (0.6-1.2s)
// Phase 6: everything slides out right + fades (1.2-1.5s)
//
// Colors: amber (#F59E0B) for "Your Turn", slate (#64748B) for "Their Turn"
```

### Death Shatter Effect Pattern
```typescript
// Implementation approach using Canvas 2D:
// 1. When CreatureDestroyed event fires, get the card's DOM position
// 2. Load the art_crop image from cache (already loaded for battlefield display)
// 3. Create offscreen canvas sized to the card element
// 4. Draw the image onto the offscreen canvas
// 5. Generate 8-12 random polygon regions (Voronoi-like subdivision)
// 6. For each fragment:
//    - Use canvas clip() to isolate the polygon region
//    - drawImage() to render that fragment
//    - Apply random velocity (outward from center), rotation, gravity, alpha decay
// 7. Animate via ParticleCanvas or dedicated requestAnimationFrame loop
// 8. Total duration: 0.6s (matches CONTEXT.md spec)
```

### Card Flight Arc (Hand to Stack)
```typescript
// Uses Framer Motion with keyframes for a curved path
// Source: hand card position (bottom center)
// Destination: stack position (right center)
// Arc: card moves up and to the right along a parabolic curve
//
// Approach: use motion.div with animate={{ x, y }} where values are
// computed via a keyframes array representing the arc path.
// Add scale pulse (1.0 -> 1.1 -> 1.0) and glow effect at destination.
```

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| Framer Motion package | Motion (rebranded from Framer Motion) | 2025 | Same API, import from `framer-motion` still works |
| `small` images on battlefield | `art_crop` for battlefield, `normal` for hand | Phase 19 | Different visual treatment per zone |
| 90-degree tap rotation | 15-20 degree tap rotation | Phase 19 | Overrides Phase 13 BOARD-05 |
| Full card images everywhere | Dual image strategy (art_crop + normal) | Phase 19 | More MTGA-faithful presentation |

## Open Questions

1. **Alchemy TurnBanner Source Code**
   - What we know: CONTEXT.md mentions "port Alchemy's TurnBanner" and "port Alchemy's menu particle background"
   - What's unclear: The Alchemy (tasmania) project directory was not found at the expected path. The TurnBanner to port may have been from a different project path or may need to be built from the description.
   - Recommendation: Build the cinematic TurnBanner from the description (layered: light burst, banner strip, diamond accents, punchy text with triple glow, amber/slate colors, 1.5s duration) rather than trying to find source code. The existing TurnBanner.tsx provides the integration point.

2. **Art-Crop Exact Container Dimensions**
   - What we know: Scryfall art_crop dimensions vary by card. Typical range is roughly 620x450 to 640x460 pixels.
   - What's unclear: Exact pixel dimensions on screen for the best MTGA feel.
   - Recommendation: Use CSS custom properties with ~4:3 aspect ratio containers (matching most art_crop images). Start with 5.5vw width on desktop and adjust during visual testing. The `object-cover` CSS property handles variation.

3. **Representative Card Art for Deck Gallery**
   - What we know: Each deck tile needs a representative card's art_crop.
   - What's unclear: Which card to choose as representative.
   - Recommendation: Use the first non-land card in the deck list (by order). If the deck is all lands, use the first card. This is simple, deterministic, and gives a meaningful visual for most decks.

## Validation Architecture

### Test Framework
| Property | Value |
|----------|-------|
| Framework | Vitest 3.x with jsdom |
| Config file | `client/vitest.config.ts` |
| Quick run command | `cd client && pnpm test -- --run` |
| Full suite command | `cd client && pnpm test -- --run --coverage` |

### Phase Requirements to Test Map

Since Phase 19 has no formally defined requirement IDs (listed as TBD), the validation targets are derived from the CONTEXT.md locked decisions:

| Area | Behavior | Test Type | Automated Command | File Exists? |
|------|----------|-----------|-------------------|-------------|
| Image infrastructure | `art_crop` size accepted by scryfall service | unit | `cd client && pnpm test -- --run -t "art_crop"` | Wave 0 |
| Image infrastructure | Dual caching (art_crop + normal) | unit | `cd client && pnpm test -- --run -t "cache"` | Covered by existing imageCache tests |
| Card presentation | ArtCropCard renders border color by color identity | unit | `cd client && pnpm test -- --run -t "ArtCropCard"` | Wave 0 |
| Card presentation | Tap rotation uses 17deg not 90deg | unit | `cd client && pnpm test -- --run -t "tap rotation"` | Wave 0 |
| Board layout | Zone order: creatures near center, lands far | unit | `cd client && pnpm test -- --run -t "zone order"` | Wave 0 |
| Animation | Turn banner renders with correct theming | unit | `cd client && pnpm test -- --run -t "TurnBanner"` | Wave 0 |
| Menu | Mode-first flow navigation | integration | Manual verification | N/A |
| Deck builder | Art-crop grid renders | unit | `cd client && pnpm test -- --run -t "CardGrid"` | Wave 0 |

### Sampling Rate
- **Per task commit:** `cd client && pnpm test -- --run`
- **Per wave merge:** `cd client && pnpm test -- --run --coverage`
- **Phase gate:** Full suite green + visual inspection of key screens

### Wave 0 Gaps
- [ ] `client/src/services/__tests__/scryfall.test.ts` -- test art_crop size variant
- [ ] `client/src/components/card/__tests__/ArtCropCard.test.tsx` -- test border color logic
- [ ] `client/src/components/board/__tests__/tapRotation.test.ts` -- verify 17deg rotation
- [ ] `client/src/viewmodel/__tests__/battlefieldLayout.test.ts` -- verify zone ordering

## Sources

### Primary (HIGH confidence)
- Project codebase: `client/src/` — all existing components, hooks, services, stores examined
- Scryfall API docs (https://scryfall.com/docs/api/images) — art_crop format: JPG, varies dimensions; normal: 488x680 JPG
- Framer Motion docs (https://motion.dev/) — confirmed AnimatePresence, layout animations, keyframes API available in v12

### Secondary (MEDIUM confidence)
- Scryfall art_crop typical dimensions: ~620x450px based on community reports and API documentation stating "varies"
- MTGA board layout reference: user's screenshot and detailed description in CONTEXT.md

### Tertiary (LOW confidence)
- Canvas 2D shatter effect: implementation pattern based on CodePen examples and Canvas clipping API documentation

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH — all libraries already in use, no new dependencies
- Architecture: HIGH — clear mapping from CONTEXT.md decisions to existing component structure
- Image infrastructure: HIGH — Scryfall API supports art_crop variant, existing caching works
- Animation effects: MEDIUM — death shatter and cast arc are new patterns, but Canvas 2D and Framer Motion are proven tools
- Board layout: HIGH — CSS flexbox restructure with fixed positioning for overlays
- Menu overhaul: HIGH — straightforward React component restructure

**Research date:** 2026-03-09
**Valid until:** 2026-04-09 (stable stack, no fast-moving dependencies)
