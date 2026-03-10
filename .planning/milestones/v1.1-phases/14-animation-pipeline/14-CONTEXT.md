# Phase 14: Animation Pipeline - Context

**Gathered:** 2026-03-08
**Status:** Ready for planning

<domain>
## Phase Boundary

Game state changes produce fluid visual feedback — particles, floating numbers, screen shake, card reveals, targeting arcs, and death animations — driven by an event-normalized animation queue. Upgrades the existing v1.0 animation scaffold (animationStore, ParticleCanvas, FloatingNumber, AnimationOverlay) into a production step-based pipeline with snapshot-before-dispatch, grouped animation steps, and configurable VFX quality/speed. Game loop, audio, and MTG-specific UI (stack viz, mana payment, combat assignment) are separate phases.

</domain>

<decisions>
## Implementation Decisions

### Death & creature persistence
- Fade + particle burst: creature fades to transparent over ~400ms while red/dark particles explode outward from its position
- Board snapshot system preserves dying creature's card image as a clone in an overlay layer before state updates remove it from the board — clone fades out with particles, then is removed from overlay
- Siblings animate closed to fill the gap after the death animation completes (Framer Motion AnimatePresence/layoutId)
- Multiple simultaneous deaths (board wipe) play all at once, not staggered — board reflows once after all death animations finish

### Particle & VFX style
- Simple circles with glow (ctx.shadowBlur), varying sizes (2-6px radius), slight gravity pull — performant, Arena-like
- WUBRG color mapping is color-only, same burst behavior for all:
  - White: #fbbf24 (warm gold)
  - Blue: #06b6d4 (cyan)
  - Black: #a855f7 (purple)
  - Red: #ef4444 (red-orange)
  - Green: #22c55e (emerald)
  - Colorless: #94a3b8 (slate)
  - Multicolor: cycle through relevant colors
- Screen shake on combat damage via CSS transform on game container:
  - Light (1-3 dmg): ±2px, 150ms
  - Medium (4-6 dmg): ±4px, 250ms
  - Heavy (7+ dmg): ±8px, 350ms
  - 4-6 oscillations with decaying amplitude
- Damage vignette: red radial gradient from edges inward, 200ms fade-in + 300ms fade-out, opacity scales with damage amount

### Animation sequencing
- Snapshot-before-dispatch flow: capture all card positions before WASM call, play animations using snapshot positions after events return, apply new board state only after all animations complete
- Event normalizer groups related events into steps — parallel within a step, sequential across steps (e.g., board wipe: SpellCast step → DamageDealt x4 step → CreatureDestroyed x4 step → state update)
- Animation speed as 4 named presets stored in preferencesStore:
  - Slow: 1.5x duration
  - Normal: 1.0x duration (default)
  - Fast: 0.5x duration
  - Instant: skip all animations (0x)
- VFX quality as 3 tiers in preferencesStore:
  - Full: all effects (particles with glow, floating numbers, screen shake, damage vignette, card reveal burst, phase banners)
  - Reduced: particles at 50% count without glow, floating numbers, card reveal burst, phase banners — no screen shake or vignette
  - Minimal: floating numbers only, phase banners as text-only — no particles, shake, vignette, or reveal bursts

### Turn/phase banners
- Center banner slide-through: text slides in from left, pauses ~600ms, slides out right on semi-transparent dark strip
- Turn start only — no banners for individual phase changes (upkeep, draw, main, combat, end step update silently via phase tracker)
- Banner text: "Turn N — Your Turn" or "Turn N — Opponent's Turn"

### Card reveal effects
- Scale up + particle burst on battlefield entry: card scales from 0.8→1.0 with WUBRG-colored particle burst from center, ~300ms total
- Particle color matches the card's color identity

### Claude's Discretion
- Event normalizer grouping heuristics (which events belong in the same step)
- Exact particle counts, decay rates, and glow intensity
- Animation easing curves and exact timing within the speed presets
- Snapshot implementation details (DOM cloning vs React portal vs canvas snapshot)
- How the async dispatch wrapper integrates with the existing gameStore dispatch flow
- Targeting arc and block assignment line styles (VFX-06, VFX-07 — existing TargetArrow and BlockerArrow components provide the base)

</decisions>

<specifics>
## Specific Ideas

- Death animation plays all at once for simultaneous deaths (not staggered) — user wants fast board wipes, not cascading delays
- Snapshot-before-dispatch is the core architectural pattern — positions captured pre-WASM, animations play on snapshot, state updates after animations complete
- WUBRG colors use warm/saturated hues (#fbbf24 gold, #06b6d4 cyan, #a855f7 purple, #ef4444 red, #22c55e emerald) — not pure RGB
- Speed presets use named levels, not continuous slider — "Slow", "Normal", "Fast", "Instant"

</specifics>

<code_context>
## Existing Code Insights

### Reusable Assets
- `animationStore.ts`: Zustand store with queue, position registry, and event→effect mapping — needs upgrade to step-based architecture
- `ParticleCanvas.tsx`: Canvas-based particle system with `emitBurst()` and `emitTrail()` — extend with glow, gravity, size variation
- `FloatingNumber.tsx`: Framer Motion damage/heal floats — works as-is, just needs VFX quality gating
- `AnimationOverlay.tsx`: Orchestrates effects for DamageDealt, LifeChanged, AttackersDeclared, CreatureDestroyed, SpellCast — refactor to step-based processing
- `TargetArrow.tsx`: SVG targeting arc with Framer Motion animation — base for VFX-07
- `BlockerArrow.tsx`: SVG blocker assignment line using DOM queries — base for VFX-06
- `preferencesStore.ts`: Zustand persist store — add `animationSpeed` and `vfxQuality` settings

### Established Patterns
- Zustand stores with selector pattern for reactive subscriptions
- Framer Motion for DOM animations (AnimatePresence, layout animations)
- HTML Canvas for particle effects (separate from React render cycle)
- `data-object-id` DOM attributes for position registry lookups
- Discriminated union GameEvent types (34 variants) as animation source

### Integration Points
- `gameStore.dispatch()`: Wraps adapter.submitAction() — needs async wrapper for snapshot-animate-update flow
- `GameEvent` union (34 types): Event normalizer input — maps to animation steps
- `AnimationOverlay` rendered in `GamePage` — overlay layer for death clones, vignette, banners
- `PreferencesModal`: Add VFX quality and animation speed controls

</code_context>

<deferred>
## Deferred Ideas

None — discussion stayed within phase scope

</deferred>

---

*Phase: 14-animation-pipeline*
*Context gathered: 2026-03-08*
