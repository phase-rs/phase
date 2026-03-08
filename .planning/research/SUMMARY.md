# Project Research Summary

**Project:** Forge.rs v1.1 -- Arena UI Port from Alchemy
**Domain:** MTG game engine frontend -- porting a polished card game UI between engines of different complexity
**Researched:** 2026-03-08
**Confidence:** HIGH

## Executive Summary

Forge.rs v1.1 ports Alchemy's polished, Arena-style card game UI onto Forge.rs's Rust/WASM MTG engine. The two projects share an identical core stack (React 19, Zustand 5, Framer Motion 12, Tailwind v4) and the same architectural patterns (discriminated union types, event-driven dispatch, Zustand stores for game/UI/animation state). The port requires zero new npm dependencies -- all new capabilities (procedural audio via Web Audio API, Canvas 2D particle VFX, responsive card sizing via CSS custom properties, PWA service worker registration) use browser-native APIs. Only version bumps on existing dependencies are needed (Vite 6->7, TypeScript 5.7->5.9, Vitest 3->4).

The recommended approach is a layered port: build an event normalization layer first (bridging Forge.rs's async WASM events to Alchemy's animation pipeline), then port the animation infrastructure (step-based queue, position registry, board snapshots), then layer visual components on top. The critical architectural insight is that Alchemy's dispatch is synchronous while Forge.rs's is async across a WASM boundary -- every animation timing assumption must account for this gap. The `dispatchWithAnimations` function is the central integration point and must be ported as an async wrapper around the existing `EngineAdapter`.

The top risks are: (1) Alchemy's fixed 5-slot board layout breaking with MTG's unbounded permanent count (token decks create 15+ creatures), (2) async dispatch timing causing stale position snapshots for death animations, (3) Alchemy's `GameEvent` shapes not matching Forge.rs's events, requiring a translation layer before the animation pipeline can process them, and (4) MTG-specific UI (stack visualization, mana payment, priority controls) having no Alchemy equivalent to port -- these must be built from scratch. All four risks have clear prevention strategies documented in the research.

## Key Findings

### Recommended Stack

The stack is already aligned between projects. No new runtime dependencies are needed. See `.planning/research/STACK.md` for full comparison.

**Core technologies (no changes):**
- React 19 + Zustand 5 + Framer Motion 12 -- identical across both projects
- Tailwind CSS v4 -- identical across both projects
- Web Audio API (browser native) -- zero-dependency audio system (synthesis + sample playback)
- Canvas 2D API (browser native) -- particle VFX system replacing Forge.rs's basic `ParticleCanvas`

**Version bumps required:**
- Vite ^6.2.0 -> ^7.3.1 (required for `@vitejs/plugin-react` v5)
- TypeScript ~5.7.0 -> ~5.9.3 (better discriminated union inference)
- Vitest ^3.0.0 -> ^4.0.18 (align with Alchemy)

**Do NOT add:** Howler.js (dead dep in Alchemy), workbox-window (unused), PeerJS (Forge.rs uses WebSocket), Cypress/Puppeteer (not needed for UI port).

### Expected Features

See `.planning/research/FEATURES.md` for full analysis with complexity ratings.

**Must have (table stakes):**
- Canvas particle VFX with WUBRG color mapping (9 effect types: explosion, projectile, spellImpact, etc.)
- Floating damage/heal numbers with per-step intermediate values
- Screen shake on combat damage (3 intensity levels)
- Responsive card sizing via CSS custom properties with media query breakpoints
- MTGA-style hand peek/expand with fan layout and drag-to-play
- Hero/Player HUD with health bar, mana pool summary, phase indicator
- Turn/phase banner overlay with animated entrance/exit
- Block assignment visualization (SVG lines between attacker/blocker pairs)
- Graveyard viewer modal
- Card entry/exit animations with burst effects

**Should have (differentiators):**
- Combat math bubbles showing P/T trade outcomes before damage
- Audio system: SFX on game events, ambient music with track rotation
- Card reveal animation on spell/creature plays
- Damage vignette (red screen flash)
- Battlefield backgrounds with WUBRG-based auto-selection
- VFX quality levels (full/reduced/minimal) and animation speed controls
- Preferences store with display/audio settings

**Defer (v2+):**
- Context-specific music (title screen, deck select)
- Element card effects on permanents (subtle, vfxLevel=full only)
- Custom UI layouts (default must be good first)
- Tutorial/coach system (explicitly out of scope per PROJECT.md)

**Anti-features (do NOT port):**
- Learning challenges, tutorial, easy-read mode, TTS narration (Alchemy educational features)
- Element theming as-is (remap to WUBRG instead)
- Adventure map / campaign mode
- Alchemy's game engine, network layer, energy crystal system

### Architecture Approach

The port preserves Forge.rs's async `EngineAdapter` abstraction while adopting Alchemy's superior animation pipeline. A new **event normalization layer** bridges the gap between Forge.rs's `GameEvent[]` (MTG-specific: `DamageDealt`, `ZoneChanged`, `LifeChanged`) and the animation system's expected format (`CREATURE_ENTERED`, `PLAYER_DAMAGED`, `CREATURE_DIED`). See `.planning/research/ARCHITECTURE.md` for full data flow diagrams and type mappings.

**Major components:**
1. **Event Normalizer (new)** -- translates Forge.rs events to animation-compatible shapes
2. **animationStore (ported from Alchemy)** -- step-based queue with board snapshots, display health tracking, position registry
3. **dispatchWithAnimations (ported + adapted)** -- async wrapper: pre-snapshot -> EngineAdapter dispatch -> event normalization -> animation grouping -> enqueue
4. **OpponentController + useGameLoop (ported + adapted)** -- AI scheduling via WASM `get_ai_action()`, auto-priority-pass driven by `WaitingFor`
5. **GameDispatchProvider (ported)** -- React context providing dispatch + controller to all components
6. **GameObject view model (new)** -- adapts Forge.rs's deep `GameObject` to flat props for Alchemy-derived components

### Critical Pitfalls

See `.planning/research/PITFALLS.md` for all 13 pitfalls with detection strategies.

1. **Fixed board slots vs. dynamic permanent count** -- Alchemy's 5-slot `CreatureSlots` breaks with MTG's unbounded permanents. Use multi-row layout with separate rows per card type and aggressive token stacking.
2. **Sync vs. async dispatch timing** -- Alchemy captures board snapshots synchronously before dispatch. Forge.rs dispatch is async (WASM boundary). Capture positions before the async call; serialize the dispatch-animate flow to prevent concurrent dispatches during animation.
3. **Type shape mismatch** -- Alchemy's flat `Permanent` vs. Forge.rs's deep `GameObject` (layer-evaluated power/toughness, counters, attachments). Create a view model mapping layer; do NOT spread `GameObject` props directly into ported components.
4. **No stack/priority/instant UI in Alchemy** -- MTG's stack, instant-speed interaction, and WUBRG mana payment have no Alchemy equivalent. These must be built from scratch, not ported.
5. **Card image loading model** -- Alchemy uses sync static asset paths; Forge.rs uses async Scryfall API with caching. Add loading skeletons to card art areas and batch-prefetch deck images at game init.

## Implications for Roadmap

Based on research, suggested phase structure:

### Phase 1: Foundation and Board Layout
**Rationale:** Everything visual depends on card sizing, board layout, and the adapter layer. This phase establishes the rendering foundation that all subsequent phases build on. Must resolve the fixed-slots vs. dynamic-permanents problem immediately.
**Delivers:** Responsive game board with multi-row permanent layout, MTGA-style hand with fan/peek/drag, Hero HUD with health bar and mana pool, preferences store, graveyard viewer.
**Addresses:** Table stakes features (responsive sizing, hand interaction, HUD, graveyard viewer). Forge.rs-specific features (multi-zone battlefield, tapped permanents, P/T display).
**Avoids:** Pitfalls 1 (fixed slots), 3 (type mismatch), 5 (image loading), 7 (phase model mismatch), 8 (WASM serialization), 9 (Alchemy concepts), 10 (legal actions), 11 (CSS collision), 12 (player ID types).
**Key work:** CSS custom properties, GameObject view model, `useCombatState()` hook, card loading skeletons, legal actions from WASM bridge.

### Phase 2: Animation Pipeline
**Rationale:** The animation system is the largest single visual upgrade and the most architecturally sensitive port. It transforms static state changes into visual experiences. Depends on Phase 1's board layout and position registry.
**Delivers:** Step-based animation queue, particle VFX (9 effect types remapped to WUBRG), floating damage/heal numbers, screen shake, card reveal overlay, damage vignette, block assignment lines, board snapshot preservation for death animations.
**Addresses:** Table stakes (particle VFX, floating numbers, screen shake, block assignment lines, card animations). Differentiators (card reveal, damage vignette, combat math bubbles).
**Avoids:** Pitfalls 2 (async dispatch timing), 6 (event shape mismatch), 13 (animation store ID formats).
**Key work:** Event normalizer, async `dispatchWithAnimations`, animationStore port, ParticleSystem port with WUBRG color constants.

### Phase 3: Game Loop and Controllers
**Rationale:** With visual rendering and animation in place, the game loop ties them together with auto-advance, AI scheduling, and controller abstraction. This phase makes the game "play itself" smoothly.
**Delivers:** OpponentController (AI via WASM, network via WebSocket), useGameLoop (auto-priority-pass, turn detection, animation-awareness), GameDispatchProvider (context-based dispatch).
**Addresses:** Differentiators (double-click to play, action feedback toasts). Core gameplay flow (auto-advance trivial phases, AI opponent timing).
**Avoids:** Pitfall 4 partially -- this phase adds auto-pass logic but Phase 5 handles the full priority UI.

### Phase 4: Audio System
**Rationale:** Audio is high-impact but fully independent of visual features. Can be built in parallel with Phase 3. Zero new dependencies -- pure Web Audio API.
**Delivers:** AudioContext singleton with SFX/music gain buses, procedural synthesis with sample playback fallback, ambient music with track rotation and cross-fade, iOS/iPadOS warm-up.
**Addresses:** Differentiators (SFX system, ambient music). Preferences (volume controls, mute toggles).
**Key work:** Port `audioContext.ts`, `sounds.ts`, `ambientMusic.ts`, `audioStore.ts`. Create .m4a sample assets for MTG events. Remap Alchemy's element-keyed sound variants to WUBRG.

### Phase 5: MTG-Specific UI Polish
**Rationale:** These features define what makes this an MTG client rather than a generic card game. They cannot be ported from Alchemy and must be designed for MTG's specific complexity. Deferred until last because the existing Forge.rs UI already handles them functionally.
**Delivers:** Arena-style stack visualization, WUBRG mana payment UI redesign (hybrid/phyrexian/X costs), priority pass/respond controls (auto-pass, full-control, smart-stop), battlefield backgrounds with WUBRG auto-selection, VFX quality levels, animation speed controls.
**Addresses:** Forge.rs-specific features (stack display, mana payment, priority controls). Differentiators (battlefield backgrounds, VFX levels, animation speed).
**Avoids:** Pitfall 4 (no stack/priority UI in Alchemy). These are new builds, not ports.

### Phase Ordering Rationale

- **Phase 1 before all others** because every visual component depends on card sizing CSS properties, the board layout model, and the adapter layer (view model, legal actions, player ID normalization).
- **Phase 2 before Phase 3** because the game loop's animation-awareness (wait for animations before advancing) requires the animation store to exist.
- **Phase 3 and Phase 4 are parallelizable** -- audio and game loop are fully independent subsystems.
- **Phase 5 last** because it builds NEW components (not ports) and the existing Forge.rs UI already handles these features functionally, just without polish.
- This ordering minimizes the risk of rework: each phase builds on a stable foundation from the previous phase.

### Research Flags

Phases likely needing deeper research during planning:
- **Phase 2 (Animation Pipeline):** The async dispatch timing is the trickiest integration challenge. The event normalizer must handle all Forge.rs event types (some have no Alchemy equivalent: `PermanentTapped`, `CounterAdded`, `TokenCreated`, `SpellCountered`). Research the full Forge.rs event catalog during phase planning.
- **Phase 5 (MTG-Specific UI):** Stack visualization, mana payment, and priority controls are new builds with no Alchemy reference. Research MTGA's UX patterns for these interactions.

Phases with standard patterns (skip research-phase):
- **Phase 1 (Foundation):** Both codebases are well-understood. CSS custom properties, responsive layouts, and view model patterns are straightforward.
- **Phase 3 (Game Loop):** Alchemy's controller/game-loop pattern ports cleanly with well-defined adaptations.
- **Phase 4 (Audio):** Alchemy's audio system is a clean, self-contained port. Web Audio API patterns are well-documented.

## Confidence Assessment

| Area | Confidence | Notes |
|------|------------|-------|
| Stack | HIGH | Direct package.json comparison of both projects. Zero ambiguity on dependencies. |
| Features | HIGH | Direct source code analysis of both projects. Every feature verified in source. |
| Architecture | HIGH | Direct analysis of dispatch flow, store patterns, type systems in both projects. |
| Pitfalls | HIGH | All pitfalls derived from concrete code-level incompatibilities, not speculation. |

**Overall confidence:** HIGH -- all research based on direct source code analysis of both Alchemy and Forge.rs. No external documentation or community sources needed.

### Gaps to Address

- **Forge.rs event catalog completeness:** The event normalizer mapping covers the most common events but the full set of Forge.rs `GameEvent` variants needs auditing during Phase 2 planning. Events like `CounterAdded`, `TokenCreated`, `SpellCountered`, `ReplacementApplied` need animation handling decisions.
- **Audio asset creation:** Alchemy has ~30+ .m4a samples organized by element. MTG equivalents need creation or sourcing -- the research identifies the architecture but not the specific audio assets needed.
- **WASM `getLegalActions()` export:** The research identifies that this is needed (Pitfall 10) but the Rust-side implementation scope is unresearched. May require changes to the engine crate's public API.
- **Token stacking UX:** The research identifies aggressive token stacking as necessary (Pitfall 1) but the specific UX (count badge, stack interaction, expand-on-click) needs design during Phase 1 planning.
- **Vite 6->7 migration:** Listed as a version bump but major version upgrades can have breaking changes. Validate plugin compatibility (`vite-plugin-wasm`, `vite-plugin-top-level-await`) during setup.

## Sources

### Primary (HIGH confidence)
- Alchemy source code (`/Users/matt/dev/alchemy/src/`) -- all component, store, audio, hook, and type files
- Forge.rs source code (`/Users/matt/dev/forge.rs/client/src/`) -- all adapter, store, component, hook, and service files
- Forge.rs Rust engine source (`/Users/matt/dev/forge.rs/crates/`) -- WASM bridge, type definitions
- Both projects' `package.json` files -- dependency comparison

### Secondary (MEDIUM confidence)
- None -- all findings from direct source analysis

### Tertiary (LOW confidence)
- None

---
*Research completed: 2026-03-08*
*Ready for roadmap: yes*
