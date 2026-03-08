# Phase 7: Platform Bridges & UI - Context

**Gathered:** 2026-03-07
**Status:** Ready for planning

<domain>
## Phase Boundary

A player can launch the app (desktop or browser), see the game board with card images, interact with all game elements through a responsive UI, build decks, and play a full game visually. Covers UI-01 through UI-11, DECK-01 through DECK-03, PLAT-01, PLAT-02, PLAT-04, QOL-01 through QOL-03.

</domain>

<decisions>
## Implementation Decisions

### Visual direction
- Arena/Alchemy style — modern web feel, not Forge's utilitarian panel layout
- Reference implementation: Alchemy project at `../alchemy` (Zustand stores, Framer Motion, Tailwind, particle VFX, glow rings)
- Full animation infrastructure: Zustand animation store, Framer Motion layout animations, combat projectiles, screen shake, particle effects
- Both players' cards face the same direction (readable without rotation) — Arena convention

### Game board layout
- Classic horizontal: opponent's board top, yours bottom, hand at very bottom
- Permanents organized in free-form type rows (creatures row, lands row, other permanents) — not stacked by name
- Stack visualization, phase tracker, life totals on side panels
- Game log visible and updating in real time

### Card rendering
- Full Scryfall card images displayed on battlefield (reduced size), zoom on hover/long-press for full-size preview
- Arena-style tapped angle (~30-45° tilt, not full 90°) to keep card art visible
- Counters as numbered badges on the card
- Attachments fan out slightly behind the permanent
- Damage shown as red overlay on toughness
- Glow ring for summoning sickness (desaturated like Arena)
- Glow rings color-coded for targeting, selection, interactability (following Alchemy patterns: cyan=target, white=interactable)

### Scryfall image strategy
- Pre-download all card images when a deck is loaded/selected (batch fetch before game starts)
- Cache to IndexedDB (PWA) or filesystem (Tauri)
- Image size: Claude's discretion (balance quality for zoom vs storage/bandwidth)
- Show card frame placeholder while loading

### Mana payment
- Auto-tap with manual override — engine's greedy algorithm (Phase 3) selects lands automatically
- Player can override by manually clicking lands to tap
- Arena-style: fast for simple casts, full control when it matters

### Targeting
- Valid targets glow with colored ring (Alchemy pattern) — click to select
- Arrow drawn from source to target
- Auto-target when exactly one legal target (engine already does this)

### Priority & game flow
- Auto-pass priority when no legal plays available
- Manual pass button when player has options
- Full Control toggle (Arena-style) — stops all auto-passing, gives priority at every step
- Keyboard shortcut for pass turn

### Modal choices & replacement ordering
- Overlay card large in center with clickable options below
- Replacement effect ordering: show competing effects as cards with "choose which applies first" prompt
- Clean modal overlay — Arena-like, not dialog boxes

### Deck builder
- Visual grid + sidebar layout: card search/filter on left, Scryfall image grid in center, deck list on right
- Mana curve chart and color distribution display
- Standard format legality filtering — show legality per card, block adding illegal cards
- Import .dck/.dec files from Forge

### Platform strategy
- PWA-first development — build everything as web app with WASM engine in browser
- Tauri desktop wrapping comes at the end — same UI, adds native window + filesystem caching
- TauriAdapter implements existing EngineAdapter interface (designed in Phase 1)
- Responsive breakpoints for tablet/touch — same React app, CSS adjusts card sizes, zone spacing, button sizes
- Long-press for card inspection on touch devices

### Claude's Discretion
- Scryfall image size selection (normal vs small vs large)
- Exact Zustand store structure (game, animation, UI stores — Alchemy patterns available as reference)
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

</decisions>

<specifics>
## Specific Ideas

- "I'm going to likely want something that looks as much like MTG Arena as possible" — Arena is the north star for visual design
- Alchemy project (`../alchemy`) is a proven reference for the web implementation approach — reuse patterns for Zustand stores, Framer Motion animations, glow rings, combat VFX, hand interaction, touch gestures
- Arena uses a smaller tapped angle (~30-45°) rather than 90° — keeps card art visible on battlefield
- Forge project (`../forge`) available as reference for game logic presentation (how zones, choices, and targeting are surfaced to the player) but not for visual style

</specifics>

<code_context>
## Existing Code Insights

### Reusable Assets
- `client/src/adapter/types.ts`: EngineAdapter interface with initialize/submitAction/getState/dispose — designed for both WASM and Tauri transports
- `client/src/adapter/wasm-adapter.ts`: Working WasmAdapter with async queue serialization
- `client/src/wasm/`: Pre-built WASM bindings with auto-generated TypeScript types via tsify
- `../alchemy/src/components/board/`: Full game board implementation (GameBoard, CreatureSlots, BoardCard) — reference for Arena-style layout
- `../alchemy/src/game/gameStore.ts`: Zustand game store pattern — reference for state management
- `../alchemy/src/components/animation/`: Animation overlay, particle system, projectile VFX — reference for combat animations

### Established Patterns
- EngineAdapter abstraction: React components call `adapter.submitAction(action)` without knowing transport (Phase 1 decision)
- tsify auto-generates TypeScript discriminated unions from Rust enums — no manual TS types for engine data
- WaitingFor state machine: engine returns waiting state, UI must present the appropriate prompt (targeting, mana payment, choices, replacement ordering)
- Build-registry-per-call pattern in engine (effects, triggers, replacements, static abilities)
- React 19 + Vite + Vitest already configured
- vite-plugin-wasm for WASM imports

### Integration Points
- `client/src/App.tsx`: Current skeleton — needs complete replacement with game UI router
- `client/src/adapter/types.ts`: GameState/GameAction/GameEvent types need expansion to match full engine state (currently placeholder subset)
- Engine's WaitingFor variants drive all UI interaction prompts
- Engine's GameEvent stream drives animation queue and game log
- Scryfall API for card images (external dependency)
- Tauri IPC for TauriAdapter (Phase 7 scope)

</code_context>

<deferred>
## Deferred Ideas

None — discussion stayed within phase scope

</deferred>

---

*Phase: 07-platform-bridges-ui*
*Context gathered: 2026-03-07*
