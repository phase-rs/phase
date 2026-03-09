# Phase 14: Animation Pipeline - Research

**Researched:** 2026-03-08
**Domain:** Animation pipeline, Canvas 2D particles, Framer Motion layout/exit animations, event-driven VFX
**Confidence:** HIGH

## Summary

Phase 14 upgrades the existing v1.0 animation scaffold (animationStore, ParticleCanvas, FloatingNumber, AnimationOverlay) into a production step-based animation pipeline. The core architectural challenge is the snapshot-before-dispatch pattern: capturing card positions before the WASM call, playing animations against those snapshot positions, then applying the new game state after all animations complete. This requires wrapping the existing `useGameDispatch` hook with async dispatch serialization and introducing an event normalizer that groups related `GameEvent` arrays into sequential animation steps.

The existing codebase already has the key building blocks: a `ParticleCanvas` with `emitBurst`/`emitTrail`, `FloatingNumber` with Framer Motion, `AnimationOverlay` that processes effects from a queue, `TargetArrow` and `BlockerArrow` SVG components, and a `preferencesStore` with `zustand/persist`. The work is primarily refactoring the flat event-to-effect mapping into a step-based pipeline, enhancing particles with glow/gravity/size variation, and adding new VFX (screen shake, vignette, card reveal bursts, turn banners).

**Primary recommendation:** Refactor animationStore from flat queue to step-based pipeline, wrap gameStore.dispatch with snapshot-capture-animate-update flow, and gate all VFX through a quality/speed multiplier read from preferencesStore.

<user_constraints>

## User Constraints (from CONTEXT.md)

### Locked Decisions
- Death animation: fade + particle burst, ~400ms, board snapshot preserves dying creature as clone in overlay layer, siblings animate closed after death completes (Framer Motion AnimatePresence/layoutId), simultaneous deaths play all at once
- Particle style: simple circles with ctx.shadowBlur glow, 2-6px radius, slight gravity pull
- WUBRG colors: White=#fbbf24, Blue=#06b6d4, Black=#a855f7, Red=#ef4444, Green=#22c55e, Colorless=#94a3b8, Multicolor=cycle
- Screen shake via CSS transform on game container: Light(1-3dmg)=+-2px/150ms, Medium(4-6dmg)=+-4px/250ms, Heavy(7+dmg)=+-8px/350ms, 4-6 oscillations with decaying amplitude
- Damage vignette: red radial gradient from edges inward, 200ms fade-in + 300ms fade-out, opacity scales with damage
- Snapshot-before-dispatch: capture positions before WASM call, animate on snapshot, apply state after animations
- Event normalizer groups related events into steps: parallel within step, sequential across steps
- Animation speed: 4 named presets (Slow=1.5x, Normal=1.0x, Fast=0.5x, Instant=0x) in preferencesStore
- VFX quality: 3 tiers (Full=all effects, Reduced=50% particles no glow/shake/vignette, Minimal=floating numbers + text banners only) in preferencesStore
- Turn banners: center slide-through on turn start only, "Turn N -- Your/Opponent's Turn"
- Card reveal: scale 0.8->1.0 with WUBRG particle burst from center, ~300ms

### Claude's Discretion
- Event normalizer grouping heuristics (which events belong in the same step)
- Exact particle counts, decay rates, and glow intensity
- Animation easing curves and exact timing within the speed presets
- Snapshot implementation details (DOM cloning vs React portal vs canvas snapshot)
- How the async dispatch wrapper integrates with the existing gameStore dispatch flow
- Targeting arc and block assignment line styles (VFX-06, VFX-07 -- existing TargetArrow and BlockerArrow provide the base)

### Deferred Ideas (OUT OF SCOPE)
None -- discussion stayed within phase scope

</user_constraints>

<phase_requirements>

## Phase Requirements

| ID | Description | Research Support |
|----|-------------|-----------------|
| ANIM-01 | Step-based animation queue processes engine events sequentially with configurable timing | Step-based pipeline architecture; speed multiplier from preferencesStore |
| ANIM-02 | Event normalizer translates GameEvent types to animation-compatible format | Event normalizer with grouping heuristics; maps 34 GameEvent variants to animation steps |
| ANIM-03 | Board snapshot system preserves dying creatures visually during death animation | React portal overlay with cloned card element; AnimatePresence exit animations |
| ANIM-04 | Async dispatch wrapper captures positions before WASM call and serializes dispatch-animate flow | Wrapping useGameDispatch with position snapshot, async gate, and deferred state update |
| ANIM-05 | VFX quality levels (full/reduced/minimal) configurable in preferences | preferencesStore extension with vfxQuality field; quality-gated VFX rendering |
| ANIM-06 | Animation speed slider configurable in preferences | preferencesStore extension with animationSpeed field; duration multiplier |
| VFX-01 | Canvas particle system with WUBRG color mapping | ParticleCanvas enhancement: glow, gravity, variable size, color mapping |
| VFX-02 | Floating damage/heal numbers animate per step | FloatingNumber already functional; needs speed multiplier and quality gating |
| VFX-03 | Screen shake at 3 intensity levels | CSS transform keyframes on game container div |
| VFX-04 | Card reveal animation on entry with burst | Framer Motion scale animation + ParticleCanvas burst at card position |
| VFX-05 | Damage vignette on player damage | CSS radial gradient overlay div with opacity animation |
| VFX-06 | Block assignment SVG lines | BlockerArrow already exists; enhance styling for VFX quality tiers |
| VFX-07 | Targeting arcs during resolution | TargetArrow already exists; integrate with animation step timing |
| VFX-08 | Turn/phase banner overlay | Framer Motion slide-through banner component |

</phase_requirements>

## Standard Stack

### Core
| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| framer-motion (Motion) | ^12.35.1 | DOM animations, AnimatePresence exit, layout transitions | Already in project; industry standard for React animation |
| Canvas 2D API | Browser native | Particle system rendering | Already used by ParticleCanvas; no library needed |
| zustand | ^5.0.11 | Animation store, preferences store | Already in project; existing pattern |

### Supporting
| Library | Version | Purpose | When to Use |
|---------|---------|---------|-------------|
| zustand/middleware (persist) | ^5.0.11 | Persist VFX/speed preferences | Already used by preferencesStore |

### Alternatives Considered
| Instead of | Could Use | Tradeoff |
|------------|-----------|----------|
| Canvas 2D particles | WebGL/Three.js particles | Overkill for <200 particles per burst; Canvas 2D is sufficient and simpler |
| CSS keyframes for shake | Framer Motion shake | CSS keyframes give more control over oscillation pattern and are cheaper |
| React portal for death clones | Canvas snapshot | React portal preserves card styling; canvas would need image re-rendering |

**Installation:**
```bash
# No new dependencies required -- all libraries already in package.json
```

## Architecture Patterns

### Recommended Project Structure
```
client/src/
├── stores/
│   ├── animationStore.ts          # Refactored: step-based pipeline
│   └── preferencesStore.ts        # Extended: vfxQuality, animationSpeed
├── hooks/
│   └── useGameDispatch.ts         # Refactored: snapshot-animate-update flow
├── animation/                     # New: animation pipeline internals
│   ├── eventNormalizer.ts         # GameEvent[] -> AnimationStep[]
│   ├── types.ts                   # AnimationStep, StepEffect, VfxQuality, AnimationSpeed
│   └── wubrgColors.ts             # WUBRG color mapping constants
├── components/
│   ├── animation/
│   │   ├── AnimationOverlay.tsx   # Refactored: step-based processing, death clones, vignette, banner
│   │   ├── ParticleCanvas.tsx     # Enhanced: glow, gravity, variable size
│   │   ├── FloatingNumber.tsx     # Enhanced: speed multiplier
│   │   ├── ScreenShake.tsx        # New: CSS transform shake on game container
│   │   ├── DamageVignette.tsx     # New: red radial gradient overlay
│   │   ├── TurnBanner.tsx         # New: slide-through turn announcement
│   │   └── CardRevealBurst.tsx    # New: scale + particle burst on entry
│   ├── combat/
│   │   └── BlockerArrow.tsx       # Existing: may need minor style updates
│   ├── targeting/
│   │   └── TargetArrow.tsx        # Existing: may need animation step integration
│   └── settings/
│       └── PreferencesModal.tsx   # Extended: VFX quality + animation speed controls
└── pages/
    └── GamePage.tsx               # Wire screen shake container ref
```

### Pattern 1: Step-Based Animation Pipeline
**What:** Events from engine are grouped into sequential steps. Each step contains parallel effects. Steps play one after another; effects within a step play simultaneously.
**When to use:** Every dispatch cycle.
**Example:**
```typescript
// animation/types.ts
export type VfxQuality = 'full' | 'reduced' | 'minimal';
export type AnimationSpeed = 'slow' | 'normal' | 'fast' | 'instant';

export const SPEED_MULTIPLIERS: Record<AnimationSpeed, number> = {
  slow: 1.5,
  normal: 1.0,
  fast: 0.5,
  instant: 0,
};

export interface StepEffect {
  type: string;
  data: unknown;
  duration: number; // base duration before speed multiplier
}

export interface AnimationStep {
  effects: StepEffect[];
  duration: number; // max of effect durations (longest wins)
}
```

### Pattern 2: Snapshot-Before-Dispatch
**What:** Before calling WASM via adapter.submitAction(), capture all card positions from the DOM position registry. After events return, play animations using snapshot positions. Only after all animation steps complete, update the game state in the store.
**When to use:** Every gameStore.dispatch call.
**Example:**
```typescript
// hooks/useGameDispatch.ts (refactored)
export function useGameDispatch() {
  return useCallback(async (action: GameAction) => {
    const { adapter, gameState } = useGameStore.getState();
    if (!adapter || !gameState) return;

    // 1. Snapshot positions before WASM call
    const snapshot = capturePositionSnapshot();

    // 2. Call WASM -- state changes inside engine
    const events = await adapter.submitAction(action);

    // 3. Normalize events into animation steps
    const steps = normalizeEvents(events);

    // 4. Play animation steps using snapshot positions
    if (steps.length > 0 && speed !== 'instant') {
      await playAnimationSteps(steps, snapshot);
    }

    // 5. Now fetch and apply new state
    const newState = await adapter.getState();
    useGameStore.setState({ gameState: newState, ... });
  }, []);
}
```

### Pattern 3: Quality-Gated VFX Rendering
**What:** Every VFX component reads `vfxQuality` from preferencesStore and conditionally renders based on tier.
**When to use:** Every VFX component.
**Example:**
```typescript
// Inside any VFX component
const vfxQuality = usePreferencesStore((s) => s.vfxQuality);

// Particles: full = all with glow, reduced = 50% no glow, minimal = none
if (vfxQuality === 'minimal') return null;
const particleCount = vfxQuality === 'reduced' ? baseCount / 2 : baseCount;
const useGlow = vfxQuality === 'full';
```

### Pattern 4: Event Normalizer Grouping Heuristics
**What:** The normalizer groups consecutive related events into the same step. Events are sequential by default but grouped when they represent the same game action.
**When to use:** Every event array from dispatch.
**Recommended grouping rules:**
```typescript
// animation/eventNormalizer.ts
// Step grouping heuristics:
// 1. SpellCast always starts a new step
// 2. Consecutive DamageDealt events group into one step (e.g., combat damage)
// 3. Consecutive CreatureDestroyed events group into one step (board wipe)
// 4. ZoneChanged events group with their preceding cause (SpellCast -> ZoneChanged)
// 5. LifeChanged groups with concurrent DamageDealt
// 6. TurnStarted/PhaseChanged are their own step (for banner)
// 7. All other events are individual steps
```

### Anti-Patterns to Avoid
- **Animating before state is ready:** Never update gameStore state before animations complete; the snapshot flow must be strictly ordered.
- **Creating new Map instances on every render:** The existing positionRegistry creates `new Map()` on each update; use ref-based mutation instead for position tracking that doesn't trigger re-renders.
- **Coupling particle count to frame rate:** Use time-based decay (delta time), not frame-based (fixed decay per frame). The existing ParticleCanvas uses fixed `decay` per frame; this should be preserved for simplicity since 60fps is the target.
- **Using shadowBlur on every particle:** `ctx.shadowBlur` is expensive. Set it once per draw cycle, not per particle. Better: apply shadow to a batch via `ctx.save()`/`ctx.restore()` bracketing.

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| Exit animations | Custom DOM removal timing | Framer Motion `AnimatePresence` + `exit` prop | Handles unmount timing, DOM cleanup, layout reflow |
| Layout reflow after removal | Manual position recalculation | Framer Motion `layout` prop on siblings | Automatic FLIP animation on layout changes |
| SVG line animation | Manual SVG path interpolation | Framer Motion `motion.line` with `pathLength` | Already used by TargetArrow/BlockerArrow |
| LocalStorage persistence | Manual JSON serialization | `zustand/middleware/persist` | Already used by preferencesStore |

**Key insight:** Framer Motion already handles the hardest parts (exit animations, layout reflow). The custom work is the step-based pipeline, event normalizer, and Canvas particle enhancements.

## Common Pitfalls

### Pitfall 1: AnimatePresence Key Management
**What goes wrong:** Exit animations don't play because the component key doesn't change or AnimatePresence doesn't wrap the conditional properly.
**Why it happens:** React fragment children or missing unique keys break AnimatePresence's unmount tracking.
**How to avoid:** Ensure every conditionally rendered motion element under AnimatePresence has a unique `key` from the game object's `id`. Death clones need their own key namespace (e.g., `death-${objectId}`).
**Warning signs:** Cards disappear instantly instead of fading out.

### Pitfall 2: Position Snapshot Stale After Layout Change
**What goes wrong:** Positions captured via `getBoundingClientRect()` become stale if the DOM reflows between snapshot and animation start.
**Why it happens:** React batches state updates; other components may reflow during the async gap.
**How to avoid:** Capture positions synchronously before the WASM call. Use a Map stored in a ref, not in Zustand state (avoids triggering re-renders during capture).
**Warning signs:** Particles/floating numbers appear at wrong positions.

### Pitfall 3: Canvas shadowBlur Performance
**What goes wrong:** Particle system drops below 60fps with glow enabled.
**Why it happens:** `ctx.shadowBlur` forces expensive per-draw compositing in the browser's 2D renderer.
**How to avoid:** Limit glow to "full" VFX quality. Keep shadow values modest (shadowBlur 4-8, not 20+). Consider using `ctx.globalCompositeOperation = 'lighter'` for additive blending as a cheaper glow alternative.
**Warning signs:** FPS drops when many particles are active; profiler shows heavy paint operations.

### Pitfall 4: Dispatch Serialization / Race Conditions
**What goes wrong:** User clicks rapidly, causing multiple dispatches to interleave -- snapshot for dispatch B captures mid-animation state of dispatch A.
**Why it happens:** The async dispatch wrapper doesn't serialize.
**How to avoid:** Use a mutex/queue pattern -- if an animation pipeline is running, new dispatches wait until it completes. A simple approach: a `ref` boolean `isAnimating` that gates entry.
**Warning signs:** Garbled animations, positions jumping, duplicate effects.

### Pitfall 5: Death Clone Z-Index Layering
**What goes wrong:** Death clone (fading creature) appears behind or in front of the wrong elements.
**Why it happens:** The overlay layer's z-index competes with modals, tooltips, and the particle canvas.
**How to avoid:** Use established z-index layering: particles at z-55 (existing), death clones at z-45 (between board z-10 and particles z-55), modals at z-50+.
**Warning signs:** Visual glitches during death animations; clones obscured or obscuring.

### Pitfall 6: Zustand Re-render Loops
**What goes wrong:** Animation state changes cause cascading re-renders across the component tree.
**Why it happens:** Subscribing to the entire animation queue array triggers on every enqueue/dequeue.
**How to avoid:** Use granular selectors. Subscribe to `isPlaying` boolean rather than the full queue array. Use `useRef` for internal pipeline state that doesn't need React reactivity. Follow the pattern from Phase 13: module-level empty array constants for stable selector returns.
**Warning signs:** Infinite re-render warnings; UI freezes during animations.

## Code Examples

### WUBRG Color Mapping
```typescript
// animation/wubrgColors.ts
import type { ManaColor } from '../adapter/types';

export const WUBRG_COLORS: Record<ManaColor | 'Colorless', string> = {
  White: '#fbbf24',
  Blue: '#06b6d4',
  Black: '#a855f7',
  Red: '#ef4444',
  Green: '#22c55e',
  Colorless: '#94a3b8',
};

export function getCardColors(colors: ManaColor[]): string[] {
  if (colors.length === 0) return [WUBRG_COLORS.Colorless];
  return colors.map((c) => WUBRG_COLORS[c]);
}
```

### Enhanced Particle with Glow and Gravity
```typescript
// ParticleCanvas enhancement sketch
interface Particle {
  x: number;
  y: number;
  vx: number;
  vy: number;
  alpha: number;
  color: string;
  decay: number;
  radius: number;   // 2-6px
  gravity: number;   // slight downward pull
}

// In tick loop:
p.vy += p.gravity;  // apply gravity
p.x += p.vx;
p.y += p.vy;
p.alpha -= p.decay;

if (useGlow) {
  ctx.shadowColor = p.color;
  ctx.shadowBlur = 6;
}
ctx.beginPath();
ctx.arc(p.x, p.y, p.radius, 0, Math.PI * 2);
ctx.fill();

// Reset shadow after batch (not per particle)
if (useGlow) {
  ctx.shadowBlur = 0;
}
```

### Screen Shake via CSS
```typescript
// components/animation/ScreenShake.tsx
// Apply CSS transform to game container via ref
function applyScreenShake(
  element: HTMLElement,
  intensity: 'light' | 'medium' | 'heavy',
  speedMultiplier: number,
) {
  const config = {
    light:  { amplitude: 2, duration: 150, oscillations: 4 },
    medium: { amplitude: 4, duration: 250, oscillations: 5 },
    heavy:  { amplitude: 8, duration: 350, oscillations: 6 },
  }[intensity];

  const totalDuration = config.duration * speedMultiplier;
  const start = performance.now();

  function shake(now: number) {
    const elapsed = now - start;
    const progress = Math.min(elapsed / totalDuration, 1);
    const decay = 1 - progress;
    const offset = Math.sin(progress * config.oscillations * Math.PI * 2)
      * config.amplitude * decay;
    element.style.transform = `translate(${offset}px, ${offset * 0.5}px)`;

    if (progress < 1) {
      requestAnimationFrame(shake);
    } else {
      element.style.transform = '';
    }
  }
  requestAnimationFrame(shake);
}
```

### Damage Vignette
```typescript
// components/animation/DamageVignette.tsx
// Radial gradient overlay that fades in/out
<motion.div
  className="pointer-events-none fixed inset-0"
  style={{
    background: 'radial-gradient(ellipse at center, transparent 40%, rgba(239,68,68,VAR) 100%)',
    zIndex: 45,
  }}
  initial={{ opacity: 0 }}
  animate={{ opacity: vignetteOpacity }}
  exit={{ opacity: 0 }}
  transition={{ duration: 0.2 * speedMultiplier }}
/>
```

### Turn Banner
```typescript
// components/animation/TurnBanner.tsx
<motion.div
  className="pointer-events-none fixed inset-x-0 top-1/2 z-50 -translate-y-1/2"
  initial={{ x: '-100%', opacity: 0 }}
  animate={{ x: '0%', opacity: 1 }}
  exit={{ x: '100%', opacity: 0 }}
  transition={{
    duration: 0.3 * speedMultiplier,
    ease: 'easeInOut',
  }}
>
  <div className="mx-auto w-full bg-black/70 py-3 text-center">
    <span className="text-xl font-bold text-white tracking-wide">
      Turn {turnNumber} — {isPlayerTurn ? "Your Turn" : "Opponent's Turn"}
    </span>
  </div>
</motion.div>
```

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| Framer Motion | Motion (rebrand) | 2025 | Same API, import from `framer-motion` still works in v12 |
| Flat event queue | Step-based pipeline | This phase | Enables grouped parallel effects + sequential ordering |
| Immediate state update | Snapshot-before-dispatch | This phase | Death animations visible before board reflow |

**Deprecated/outdated:**
- None relevant. Framer Motion v12 is current. Canvas 2D API is stable.

## Open Questions

1. **Death clone implementation: DOM clone vs React portal**
   - What we know: Need to preserve card visual at its pre-dispatch position while the real card is removed from state
   - What's unclear: DOM `cloneNode()` loses React event handlers but is simpler; React portal maintains component tree but needs careful state management
   - Recommendation: Use React portal with absolute positioning. Create a `DeathClone` component that receives snapshot position + card name, renders a `CardImage` at fixed position, and fades out. Simpler than DOM cloning and preserves styling.

2. **Dispatch serialization with AI controller**
   - What we know: AI controller calls dispatch in a loop; human player can also dispatch during AI think time
   - What's unclear: Whether the animation queue should block AI dispatches or let them queue up
   - Recommendation: Use a dispatch mutex. AI controller already has a polling loop; it will naturally wait for the mutex to release.

## Validation Architecture

### Test Framework
| Property | Value |
|----------|-------|
| Framework | Vitest 3.x + jsdom |
| Config file | `client/vitest.config.ts` |
| Quick run command | `cd client && pnpm test -- --run --reporter=verbose` |
| Full suite command | `cd client && pnpm test -- --run` |

### Phase Requirements -> Test Map
| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| ANIM-01 | Step pipeline processes steps sequentially | unit | `cd client && pnpm test -- --run src/stores/__tests__/animationStore.test.ts` | Needs update (Wave 0) |
| ANIM-02 | Event normalizer groups events into steps | unit | `cd client && pnpm test -- --run src/animation/__tests__/eventNormalizer.test.ts` | Wave 0 |
| ANIM-03 | Death snapshot preserves position data | unit | `cd client && pnpm test -- --run src/animation/__tests__/eventNormalizer.test.ts` | Wave 0 |
| ANIM-04 | Async dispatch serializes and gates | unit | `cd client && pnpm test -- --run src/hooks/__tests__/useGameDispatch.test.ts` | Wave 0 |
| ANIM-05 | VFX quality stored/retrieved correctly | unit | `cd client && pnpm test -- --run src/stores/__tests__/preferencesStore.test.ts` | Exists, extend |
| ANIM-06 | Animation speed stored/retrieved correctly | unit | `cd client && pnpm test -- --run src/stores/__tests__/preferencesStore.test.ts` | Exists, extend |
| VFX-01 | WUBRG color mapping returns correct colors | unit | `cd client && pnpm test -- --run src/animation/__tests__/wubrgColors.test.ts` | Wave 0 |
| VFX-02 | FloatingNumber respects speed multiplier | unit | manual-only (Framer Motion visual) | N/A |
| VFX-03 | Screen shake intensity mapping | unit | `cd client && pnpm test -- --run src/animation/__tests__/screenShake.test.ts` | Wave 0 |
| VFX-04 | Card reveal burst triggers on ZoneChanged to Battlefield | unit | covered by eventNormalizer test | Wave 0 |
| VFX-05 | Vignette opacity scales with damage | unit | manual-only (CSS visual) | N/A |
| VFX-06 | Block assignment lines render for pairs | unit | `cd client && pnpm test -- --run src/components/combat/__tests__/CombatOverlay.test.tsx` | Exists |
| VFX-07 | Targeting arcs connect source to target | manual-only | manual-only (SVG visual) | N/A |
| VFX-08 | Turn banner shows on TurnStarted event | unit | `cd client && pnpm test -- --run src/animation/__tests__/eventNormalizer.test.ts` | Wave 0 |

### Sampling Rate
- **Per task commit:** `cd client && pnpm test -- --run`
- **Per wave merge:** `cd client && pnpm test -- --run && cd client && pnpm run type-check`
- **Phase gate:** Full suite green + type-check before `/gsd:verify-work`

### Wave 0 Gaps
- [ ] `client/src/animation/__tests__/eventNormalizer.test.ts` -- covers ANIM-02, ANIM-03, VFX-04, VFX-08
- [ ] `client/src/animation/__tests__/wubrgColors.test.ts` -- covers VFX-01
- [ ] `client/src/animation/__tests__/screenShake.test.ts` -- covers VFX-03 (intensity mapping logic only)
- [ ] `client/src/hooks/__tests__/useGameDispatch.test.ts` -- covers ANIM-04 (dispatch serialization)
- [ ] Extend `client/src/stores/__tests__/preferencesStore.test.ts` -- covers ANIM-05, ANIM-06
- [ ] Update `client/src/stores/__tests__/animationStore.test.ts` if store is refactored -- covers ANIM-01

## Sources

### Primary (HIGH confidence)
- Codebase inspection: animationStore.ts, ParticleCanvas.tsx, FloatingNumber.tsx, AnimationOverlay.tsx, useGameDispatch.ts, gameStore.ts, preferencesStore.ts, TargetArrow.tsx, BlockerArrow.tsx, adapter/types.ts, GamePage.tsx, PreferencesModal.tsx
- [Framer Motion / Motion docs](https://motion.dev/docs/react-animate-presence) - AnimatePresence, exit animations, layout prop
- [MDN Canvas API](https://developer.mozilla.org/en-US/docs/Web/API/Canvas_API/Tutorial/Optimizing_canvas) - Canvas optimization, shadowBlur performance
- [MDN shadowBlur](https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D/shadowBlur) - Performance characteristics

### Secondary (MEDIUM confidence)
- [CSS-Tricks Shake Animation](https://css-tricks.com/snippets/css/shake-css-keyframe-animation/) - CSS shake pattern
- [Motion docs](https://motion.dev/docs/react-layout-animations) - Layout animation FLIP mechanics

### Tertiary (LOW confidence)
- None

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH - all libraries already in project, no new dependencies
- Architecture: HIGH - existing code thoroughly inspected, patterns well understood
- Pitfalls: HIGH - drawn from direct codebase analysis and established browser API behavior

**Research date:** 2026-03-08
**Valid until:** 2026-04-08 (stable domain, no fast-moving dependencies)
