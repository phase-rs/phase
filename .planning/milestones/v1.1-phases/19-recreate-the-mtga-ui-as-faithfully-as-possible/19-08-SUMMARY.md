---
phase: 19-recreate-the-mtga-ui-as-faithfully-as-possible
plan: 08
subsystem: ui
tags: [react, scryfall, art-crop, deck-builder, card-preview]

requires:
  - phase: 19-01
    provides: Art-crop image infrastructure and Scryfall art_crop URL patterns
  - phase: 19-02
    provides: CardPreview instant render (no animation delay)
provides:
  - Art-crop image grid for deck builder search results
  - Card count overlay badges on grid tiles
  - Hover-to-preview integration for CardGrid and DeckList
  - CardPreview overlay in DeckBuilderPage
affects: []

tech-stack:
  added: []
  patterns: [art_crop URL extraction for ScryfallCard, onCardHover callback prop pattern]

key-files:
  created: []
  modified:
    - client/src/components/deck-builder/CardGrid.tsx
    - client/src/components/deck-builder/DeckBuilder.tsx
    - client/src/components/deck-builder/DeckList.tsx
    - client/src/pages/DeckBuilderPage.tsx

key-decisions:
  - "CardSearch already provides WUBRG color filters and type dropdown -- no duplicate filter UI added"
  - "Art crop URL extracted directly from ScryfallCard.image_uris rather than useCardImage hook"
  - "Hover state lifted from DeckBuilder to DeckBuilderPage via onCardHover callback prop"

patterns-established:
  - "onCardHover callback pattern: components emit card name on mouse enter/leave for preview"

requirements-completed: [ARENA-11]

duration: 2min
completed: 2026-03-09
---

# Phase 19 Plan 08: Deck Builder Visual Upgrade Summary

**Art-crop image grid with 4:3 tiles, card count overlays, and instant hover-to-preview in deck builder**

## Performance

- **Duration:** 2 min
- **Started:** 2026-03-09T22:59:21Z
- **Completed:** 2026-03-09T23:01:39Z
- **Tasks:** 2
- **Files modified:** 4

## Accomplishments
- CardGrid renders art_crop images at 4:3 aspect ratio instead of small card images at standard ratio
- Card name labels always visible (no hover-only reveal), with optional card count badge overlay
- Hovering any card in grid or deck list shows instant full-card CardPreview overlay on right side

## Task Commits

Each task was committed atomically:

1. **Task 1: Art-crop CardGrid + deck count overlays** - `81df22c` (feat)
2. **Task 2: DeckBuilder preview panel + color/type filter bar** - `b000e80` (feat)

## Files Created/Modified
- `client/src/components/deck-builder/CardGrid.tsx` - Art-crop images, 4:3 ratio, onCardHover callback, cardCounts badge overlay
- `client/src/components/deck-builder/DeckBuilder.tsx` - Added DeckBuilderProps with onCardHover, wired to CardGrid and DeckList
- `client/src/components/deck-builder/DeckList.tsx` - Added onCardHover prop through CardEntryRow and SectionList for hover preview
- `client/src/pages/DeckBuilderPage.tsx` - Manages hoveredCardName state, renders CardPreview overlay

## Decisions Made
- CardSearch already provides WUBRG color filters and type dropdown, so no duplicate filter bar was added (follows DRY principle and plan instruction "If CardSearch already handles some filters, integrate with its existing filter state")
- Art crop URL extracted directly from ScryfallCard.image_uris fields rather than using the useCardImage hook (which does a separate Scryfall API fetch), since CardGrid already has the full ScryfallCard objects with URLs
- Hover state lifted to DeckBuilderPage rather than kept internal to DeckBuilder, enabling CardPreview to render as a sibling overlay

## Deviations from Plan
None - plan executed exactly as written.

## Issues Encountered
None

## User Setup Required
None - no external service configuration required.

## Next Phase Readiness
- Deck builder now has visual parity with the battlefield's art-crop card display
- CardPreview integration complete for both search results and deck list
- All phase 19 plans complete

---
*Phase: 19-recreate-the-mtga-ui-as-faithfully-as-possible*
*Completed: 2026-03-09*
