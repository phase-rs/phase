---
phase: 07-platform-bridges-ui
plan: 01
subsystem: ui
tags: [tailwind, zustand, react-router, wasm, framer-motion, typescript]

requires:
  - phase: 06-advanced-rules
    provides: Complete engine with replacement effects and layer system
provides:
  - Tailwind v4 CSS-first configuration with responsive card sizing
  - Three Zustand stores (game, UI, animation) with typed interfaces
  - Full TypeScript types matching engine serde output
  - WASM bindings with submit_action/get_game_state/initialize_game
  - React Router with Menu/Game/DeckBuilder pages
affects: [07-02, 07-03, 07-04, 07-05, 07-06, 07-07, 07-08]

tech-stack:
  added: [zustand, framer-motion, react-router, tailwindcss, "@tailwindcss/vite", idb-keyval, getrandom]
  patterns: [zustand-subscribeWithSelector, css-first-tailwind-v4, discriminated-union-types, thread-local-wasm-state]

key-files:
  created:
    - client/src/index.css
    - client/src/stores/gameStore.ts
    - client/src/stores/uiStore.ts
    - client/src/stores/animationStore.ts
    - client/src/pages/MenuPage.tsx
    - client/src/pages/GamePage.tsx
    - client/src/pages/DeckBuilderPage.tsx
  modified:
    - client/src/adapter/types.ts
    - client/src/adapter/wasm-adapter.ts
    - client/src/App.tsx
    - client/vite.config.ts
    - crates/engine-wasm/src/lib.rs
    - crates/engine-wasm/Cargo.toml

key-decisions:
  - "Tailwind v4 CSS-first: @import tailwindcss in index.css, no tailwind.config"
  - "Thread-local RefCell<Option<GameState>> for WASM game state management"
  - "Import apply function directly to avoid name collision with engine crate"
  - "Disabled wasm-opt due to validation errors with current wasm-pack version"
  - "getrandom wasm_js feature required for wasm32-unknown-unknown target"
  - "ManaPool as Vec<ManaUnit> in types.ts matching engine's actual serialization"

patterns-established:
  - "Zustand stores: subscribeWithSelector for gameStore, plain create for UI/animation"
  - "Engine types mirrored as discriminated unions with type/data tags matching serde output"
  - "Animation effects derived from GameEvent types with configurable durations"
  - "Page shells as simple function components with Tailwind classes"

requirements-completed: [PLAT-02]

duration: 11min
completed: 2026-03-08
---

# Phase 7 Plan 01: Client Foundation Summary

**Tailwind v4 CSS-first setup, Zustand stores (game/UI/animation), full engine type mirrors, expanded WASM API, and react-router with 3 page shells**

## Performance

- **Duration:** 11 min
- **Started:** 2026-03-08T07:24:13Z
- **Completed:** 2026-03-08T07:35:37Z
- **Tasks:** 3
- **Files modified:** 16

## Accomplishments
- Installed all Phase 7 dependencies and configured Tailwind v4 with CSS-first approach
- Expanded WASM bindings with initialize_game, submit_action, get_game_state exports using thread-local state
- Created comprehensive TypeScript types (250+ lines) mirroring all engine serde output including GameState, GameObject, WaitingFor (9 variants), GameEvent (32 variants), GameAction (11 variants)
- Built three Zustand stores with typed interfaces: gameStore (dispatch/undo/reset), uiStore (selection/targeting), animationStore (effect queue)
- Set up react-router with Menu, Game, and DeckBuilder pages

## Task Commits

Each task was committed atomically:

1. **Task 1: Install dependencies, configure Tailwind v4, and set up CSS** - `a8b4798` (feat)
2. **Task 2: Expand WASM bindings and update WasmAdapter** - `b320768` (feat)
3. **Task 3: Expand adapter types, create Zustand stores, set up router with page shells** - `4b498c0` (feat)

## Files Created/Modified
- `client/src/index.css` - Tailwind v4 CSS-first entry with responsive card sizing custom properties
- `client/src/adapter/types.ts` - Full TypeScript types matching engine serde output
- `client/src/adapter/wasm-adapter.ts` - Updated to use submit_action/get_game_state/initialize_game
- `client/src/stores/gameStore.ts` - Game state management with EngineAdapter integration
- `client/src/stores/uiStore.ts` - UI selection/hover/targeting/inspection state
- `client/src/stores/animationStore.ts` - Animation queue with event-to-effect mapping
- `client/src/pages/MenuPage.tsx` - Menu with New Game and Deck Builder navigation
- `client/src/pages/GamePage.tsx` - Game board placeholder
- `client/src/pages/DeckBuilderPage.tsx` - Deck builder placeholder
- `client/src/App.tsx` - React Router shell with dark theme
- `client/vite.config.ts` - Added @tailwindcss/vite plugin
- `crates/engine-wasm/src/lib.rs` - Added initialize_game, submit_action, get_game_state exports
- `crates/engine-wasm/Cargo.toml` - Added getrandom wasm_js feature, disabled wasm-opt

## Decisions Made
- Tailwind v4 CSS-first: No tailwind.config.js needed, just `@import "tailwindcss"` in CSS
- Thread-local `RefCell<Option<GameState>>` for WASM state management (single-threaded constraint)
- Import `apply` function directly instead of `use engine::game::engine` to avoid name collision with `engine` crate
- Disabled wasm-opt in Cargo.toml due to validation errors with current wasm-pack/binaryen version
- Added `getrandom = { version = "0.3", features = ["wasm_js"] }` to engine-wasm for wasm32 compatibility
- ManaPool typed as `{ mana: ManaUnit[] }` matching the actual engine serialization format

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Added getrandom wasm_js feature for wasm32 compilation**
- **Found during:** Task 2
- **Issue:** getrandom 0.3 fails to compile for wasm32-unknown-unknown without wasm_js feature
- **Fix:** Added `getrandom = { version = "0.3", features = ["wasm_js"] }` to engine-wasm/Cargo.toml
- **Files modified:** crates/engine-wasm/Cargo.toml
- **Verification:** cargo build succeeds for wasm32-unknown-unknown target
- **Committed in:** b320768

**2. [Rule 3 - Blocking] Disabled wasm-opt due to validation errors**
- **Found during:** Task 2
- **Issue:** wasm-opt binary fails with "Fatal: error validating input" on the generated WASM
- **Fix:** Added `[package.metadata.wasm-pack.profile.release] wasm-opt = false` to Cargo.toml
- **Files modified:** crates/engine-wasm/Cargo.toml
- **Verification:** wasm-pack build --dev succeeds
- **Committed in:** b320768

**3. [Rule 1 - Bug] Fixed engine module name collision in WASM bindings**
- **Found during:** Task 2
- **Issue:** `use engine::game::engine;` then `engine::apply()` fails because `engine` resolves to module not crate
- **Fix:** Changed to `use engine::game::engine::apply;` and call `apply()` directly
- **Files modified:** crates/engine-wasm/src/lib.rs
- **Verification:** cargo build -p engine-wasm --target wasm32-unknown-unknown succeeds
- **Committed in:** b320768

---

**Total deviations:** 3 auto-fixed (1 bug, 2 blocking)
**Impact on plan:** All auto-fixes necessary for WASM compilation. No scope creep.

## Issues Encountered
- wasm-pack was not installed; installed via `cargo install wasm-pack`
- WASM build required resolving getrandom + wasm-opt + name collision issues (all handled as deviations above)

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- All Phase 7 dependencies installed and importable
- Zustand stores ready for component integration in Plans 02-08
- TypeScript types ready for all engine interactions
- WASM API supports full game lifecycle (initialize, submit action, get state)
- React Router navigates between Menu, Game, and DeckBuilder pages

---
*Phase: 07-platform-bridges-ui*
*Completed: 2026-03-08*
