---
phase: 19-recreate-the-mtga-ui-as-faithfully-as-possible
plan: 01
subsystem: ui
tags: [react, scryfall, art-crop, tailwind, zustand, webp]

requires:
  - phase: 13-foundation-board-layout
    provides: PTBox component, cardProps viewmodel, CSS card variables
provides:
  - ArtCropCard component for MTGA-style battlefield rendering
  - art_crop image size support across Scryfall service, image cache, and useCardImage hook
  - Art-crop CSS custom properties with responsive breakpoints
  - BattlefieldCardDisplay and TapRotation preferences
  - Forge.rs WebP logo asset
affects: [19-02, 19-03, 19-04, 19-05, 19-06]

tech-stack:
  added: []
  patterns: [art-crop-card-rendering, token-detection-via-card-id-zero]

key-files:
  created:
    - client/src/components/card/ArtCropCard.tsx
    - client/public/logo.webp
  modified:
    - client/src/services/scryfall.ts
    - client/src/services/imageCache.ts
    - client/src/hooks/useCardImage.ts
    - client/src/index.css
    - client/src/stores/preferencesStore.ts

key-decisions:
  - "Tokens detected via card_id === 0 (no is_token field on GameObject)"
  - "Art crop aspect ratio 0.75 (width:height) matches Scryfall art_crop format"
  - "ImageSize type exported from scryfall.ts for reuse"

patterns-established:
  - "ArtCropCard pattern: objectId prop, read object from gameStore, color-coded border by WUBRG identity"
  - "Token border convention: 2px for tokens vs 3px for regular cards"

requirements-completed: [ARENA-01, ARENA-02, ARENA-12]

duration: 2min
completed: 2026-03-09
---

# Phase 19 Plan 01: Art-Crop Card & Image Infrastructure Summary

**ArtCropCard component with WUBRG color-coded borders, art_crop Scryfall image support end-to-end, and responsive CSS custom properties**

## Performance

- **Duration:** 2 min
- **Started:** 2026-03-09T22:53:46Z
- **Completed:** 2026-03-09T22:56:25Z
- **Tasks:** 2
- **Files modified:** 7

## Accomplishments
- Extended Scryfall image infrastructure to support art_crop variant across service, cache, and hook layers
- Created ArtCropCard component with color-coded borders (WUBRG/gold/gray), name label, P/T overlay, loyalty shield, counter badges
- Added battlefieldCardDisplay and tapRotation display preferences to preferencesStore
- Converted Forge.rs logo to WebP format for splash/menu use

## Task Commits

Each task was committed atomically:

1. **Task 1: Extend image infrastructure for art_crop + convert logo** - `ca7a0fc` (feat)
2. **Task 2: Create ArtCropCard component with color borders, name label, and overlays** - `d6c954a` (feat)

## Files Created/Modified
- `client/src/components/card/ArtCropCard.tsx` - Art crop battlefield card renderer with WUBRG borders, P/T, loyalty, counters
- `client/src/services/scryfall.ts` - Extended ImageSize type with art_crop, exported type
- `client/src/services/imageCache.ts` - Updated getCachedImage size parameter to include art_crop
- `client/src/hooks/useCardImage.ts` - Updated UseCardImageOptions.size to include art_crop
- `client/src/index.css` - Added art-crop CSS custom properties with responsive breakpoints
- `client/src/stores/preferencesStore.ts` - Added BattlefieldCardDisplay and TapRotation preferences
- `client/public/logo.webp` - Forge.rs logo in WebP format (55KB)

## Decisions Made
- Tokens detected via `card_id === 0` since `is_token` field does not exist on the `GameObject` type
- Art crop aspect ratio uses 0.75 (width:height) to match Scryfall's art_crop format
- ImageSize type exported from scryfall.ts to enable reuse by other modules

## Deviations from Plan

None - plan executed exactly as written.

## Issues Encountered
- macOS `sips` does not support WebP output format; used `cwebp` (available via Homebrew) instead

## User Setup Required

None - no external service configuration required.

## Next Phase Readiness
- ArtCropCard component ready to wire into PermanentCard (plan 19-02)
- All image infrastructure supports art_crop end-to-end
- Display preferences ready for settings UI integration

---
*Phase: 19-recreate-the-mtga-ui-as-faithfully-as-possible*
*Completed: 2026-03-09*
