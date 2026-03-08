---
phase: 07-platform-bridges-ui
plan: 08
subsystem: ui, platform
tags: [keyboard-shortcuts, undo, pwa, tauri, service-worker, zustand]

requires:
  - phase: 07-platform-bridges-ui
    provides: "EngineAdapter interface, WasmAdapter, game/ui stores, GamePage, MenuPage"
provides:
  - "Keyboard shortcuts (Space/Enter, F, Z, T, Escape) for game interaction"
  - "Undo system for unrevealed-information actions with 5-entry history"
  - "Card coverage dashboard showing supported effects/triggers/keywords"
  - "TauriAdapter for desktop app via Tauri v2 IPC"
  - "PWA manifest and service worker for browser installation and offline caching"
  - "Platform auto-detection (createAdapter) selecting TauriAdapter or WasmAdapter"
affects: []

tech-stack:
  added: [vite-plugin-pwa, workbox]
  patterns: [dynamic-import-for-platform-adapters, keyboard-shortcut-hook, undo-history-with-action-filtering]

key-files:
  created:
    - client/src/hooks/useKeyboardShortcuts.ts
    - client/src/components/controls/CardCoverageDashboard.tsx
    - client/src/adapter/tauri-adapter.ts
    - client/public/manifest.json
  modified:
    - client/src/stores/gameStore.ts
    - client/src/adapter/index.ts
    - client/src/pages/GamePage.tsx
    - client/src/pages/MenuPage.tsx
    - client/vite.config.ts

key-decisions:
  - "Dynamic Function() import for TauriAdapter to avoid tsc bundling @tauri-apps/api in web builds"
  - "CacheFirst for Scryfall images (2000 entries, 7-day TTL), NetworkFirst for Scryfall API"
  - "Undo limited to unrevealed-info actions (PassPriority, DeclareAttackers, DeclareBlockers, ActivateAbility)"

patterns-established:
  - "Keyboard shortcut hook: useEffect with global keydown, skip input fields, access stores via getState()"
  - "Platform adapter factory: createAdapter() with dynamic import for platform-specific code"

requirements-completed: [QOL-01, QOL-02, QOL-03, PLAT-01, PLAT-02]

duration: 4min
completed: 2026-03-08
---

# Phase 7 Plan 8: QoL & Platform Bridges Summary

**Keyboard shortcuts (Space/Enter/F/Z/T/Escape), undo for unrevealed-info actions, card coverage dashboard, TauriAdapter via IPC, PWA manifest with Scryfall image caching, and platform auto-detection**

## Performance

- **Duration:** 4 min
- **Started:** 2026-03-08T08:00:57Z
- **Completed:** 2026-03-08T08:05:00Z
- **Tasks:** 2
- **Files modified:** 13

## Accomplishments
- Keyboard shortcuts for all game actions (pass, full control, undo, tap all, cancel) with input field guard
- Undo system with 5-entry history limited to unrevealed-information actions only
- Card coverage dashboard with searchable tabs for 39 effects, 27 triggers, 50+ keywords
- TauriAdapter implementing EngineAdapter via dynamic Tauri v2 IPC invoke
- PWA manifest + service worker with CacheFirst for Scryfall card images
- Platform auto-detection via createAdapter() checking window.__TAURI_INTERNALS__
- Real store tests (gameStore + uiStore) replacing todo stubs

## Task Commits

Each task was committed atomically:

1. **Task 1: Keyboard shortcuts, undo system, card coverage dashboard** - `df9ae96` (feat)
2. **Task 2: TauriAdapter, PWA manifest, platform auto-detection** - `9424716` (feat)

## Files Created/Modified
- `client/src/hooks/useKeyboardShortcuts.ts` - Global keyboard shortcut handler with input guard
- `client/src/components/controls/CardCoverageDashboard.tsx` - Searchable effect/trigger/keyword coverage display
- `client/src/adapter/tauri-adapter.ts` - Tauri v2 IPC engine adapter with dynamic import
- `client/src/adapter/index.ts` - Platform auto-detection factory (createAdapter)
- `client/src/stores/gameStore.ts` - Enhanced with undoable action filtering and history cap
- `client/src/stores/__tests__/gameStore.test.ts` - 9 real tests for init, dispatch, undo, reset
- `client/src/stores/__tests__/uiStore.test.ts` - 8 real tests for UI state management
- `client/src/pages/GamePage.tsx` - Wired useKeyboardShortcuts hook
- `client/src/pages/MenuPage.tsx` - Added Card Coverage button with modal
- `client/public/manifest.json` - PWA manifest with standalone display
- `client/vite.config.ts` - Added VitePWA plugin with Scryfall caching

## Decisions Made
- Used Function() constructor for dynamic import of @tauri-apps/api/core to avoid TypeScript compile errors in web builds (no @tauri-apps/api dependency installed)
- CacheFirst strategy for Scryfall images (rarely change), NetworkFirst for API data (may update)
- Undo only tracks unrevealed-information actions; PlayLand, CastSpell etc. clear no history since they reveal game info

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 3 - Blocking] Dynamic import via Function() for TauriAdapter**
- **Found during:** Task 2
- **Issue:** `import("@tauri-apps/api/core")` caused tsc build failure since @tauri-apps/api is not installed
- **Fix:** Used `Function('return import("@tauri-apps/api/core")')()` to bypass static analysis
- **Files modified:** client/src/adapter/tauri-adapter.ts
- **Verification:** `pnpm run build` succeeds
- **Committed in:** 9424716

---

**Total deviations:** 1 auto-fixed (1 blocking)
**Impact on plan:** Essential fix for building without Tauri dependency. No scope creep.

## Issues Encountered
None beyond the auto-fixed deviation above.

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- All Phase 7 plans complete
- Full platform support: WASM for browser, Tauri IPC adapter for desktop (backend scaffolding separate)
- PWA installable from browser with offline image caching
- Quality-of-life features make game playable with keyboard

---
*Phase: 07-platform-bridges-ui*
*Completed: 2026-03-08*
