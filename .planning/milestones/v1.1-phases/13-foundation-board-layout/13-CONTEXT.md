# Phase 13: Foundation & Board Layout - Context

**Gathered:** 2026-03-08
**Status:** Ready for planning

<domain>
## Phase Boundary

Replace v1.0's functional game UI with a polished Arena-style board. Responsive battlefield with permanent grouping, fan hand with drag-to-play, floating HUDs, zone viewer modals, collapsible game log, view model layer, and preferences infrastructure. All wired through the existing EngineAdapter interface. Animation pipeline, audio, stack/priority UI, and mana payment UI are separate phases.

</domain>

<decisions>
## Implementation Decisions

### Board layout
- Three-row battlefield grouped by type: creatures top, non-creature permanents middle, lands bottom — MTGA style
- CSS custom properties for responsive card sizing: `--card-w` / `--card-h` scaling with viewport via media queries (desktop ~7vw, tablet ~12vw, mobile ~18vw)
- Same-name tokens stack with count badge, expandable on click to show individuals
- Same-name lands group together with count badge, same expand behavior
- Auras/equipment tuck behind host permanent with top edge visible, click host to see full attachments
- Tapped permanents rotate 90° clockwise, no opacity change — matches Arena exactly
- WUBRG-based battlefield background auto-selection from Forge assets based on player's dominant mana color, with neutral fallback for multicolor

### Power/toughness & counters
- Arena-style P/T box showing MODIFIED values (not base)
- Green text when power or toughness is buffed above base (e.g. +1/+1 counters, pump effects)
- Red text on toughness when damaged (toughness displayed as effective = toughness - damage_marked)
- Red text on both P/T when debuffed below base (e.g. -1/-1 counters)
- Small counter badge separately showing +1/+1 or -1/-1 count
- Loyalty displayed in shield badge on planeswalkers
- Engine already provides `power`/`toughness` (modified) vs `base_power`/`base_toughness` — compare to determine highlight color

### Hand interaction
- MTGA-style peek fan from bottom edge: cards peek in collapsed state, expand upward on hover
- Drag-to-play with 50px minimum drag threshold to prevent accidental plays
- Playable cards highlighted with green glow border based on legal actions from engine
- Non-playable cards slightly dimmed
- Hover preview shows full-size card image to the side (left or right of cursor)
- Opponent hand: compact card backs fanned horizontally at top edge
- Touch: tap hand zone to expand, tap card to select (shows preview), tap again or drag to play, long-press for full preview, tap elsewhere to deselect & collapse

### Overall page layout
- Full-screen board with no permanent side panel — board fills entire viewport
- Player HUD inline with hand zone at bottom, opponent HUD inline at top
- Phase indicator between the two battlefields (divider area)
- Game log: slide-out panel from right edge with toggle button, overlays board when open
- Zone viewers (graveyard, exile): modal overlays with scrollable card grid, triggered by clicking zone count indicators
- Settings gear icon in player HUD opens preferences modal during gameplay
- HUD positioning built as standalone component — architecturally flexible for A/B testing between inline-with-hand and floating-corners layouts via preference toggle

### Game log
- Color-coded entries by event type: combat (red), spells (blue/purple), life changes (green/red), zone changes (gray)
- Verbosity filter with three levels: full, compact, minimal
- Scrollable with auto-scroll to latest entry

### Preferences infrastructure
- New dedicated `preferencesStore` (Zustand with `persist` middleware to localStorage)
- Separate from `uiStore` (ephemeral) — preferences are persistent across sessions
- Settings: card size scaling (small/medium/large), HUD layout mode (inline/floating), game log default state (open/closed), board background theme (auto-WUBRG/specific color/none)
- Accessible via gear icon in player HUD during gameplay

### View model
- Claude's Discretion: hybrid approach — pure mapping functions for data transformation (e.g. `toCardProps(obj)`) consumed via lightweight Zustand selectors for reactivity
- Components never touch raw engine types directly
- Idiomatic Zustand pattern: testable standalone functions + reactive selectors via `subscribeWithSelector`

### Claude's Discretion
- Exact CSS custom property values and breakpoint thresholds
- Card aspect ratio and border radius
- Counter badge exact positioning and sizing
- Drag animation easing and snap-back behavior
- Log panel width and animation timing
- Settings modal layout and control types
- View model function signatures and selector granularity

</decisions>

<specifics>
## Specific Ideas

- "Let's try to go MTGA style" — Arena is the primary visual reference for all layout decisions
- "Doesn't MTGA modify the actual A/T values and highlight them?" — Yes, P/T box shows modified values with color coding, not base + modifier
- "Can we see what Alchemy does and make HUD toggleable for A/B testing?" — HUD positioning should be configurable between layout modes
- Tapped rotation matches Arena exactly: 90° clockwise, no dimming

</specifics>

<code_context>
## Existing Code Insights

### Reusable Assets
- `GamePage.tsx`: Full game layout with working mulligan, combat, targeting, mana payment overlays — restructure for full-screen layout
- `gameStore.ts`: Zustand store with `subscribeWithSelector` middleware — pattern for new `preferencesStore`
- `uiStore.ts`: Ephemeral UI state (selection, hover, targeting, combat) — stays separate from preferences
- `adapter/types.ts`: Full type system with `GameObject` (has `power`/`base_power`, `toughness`/`base_toughness`, `tapped`, `counters`, `attached_to`, `attachments`, `damage_marked`)
- `useCardImage` hook + `scryfall.ts` + `imageCache.ts`: Card image loading pipeline (IndexedDB caching)
- `useLongPress` hook: Touch long-press detection — reusable for touch card preview
- `CardImage`, `CardPreview`: Existing card display components
- `ChoiceModal`: Existing modal pattern for zone viewers

### Established Patterns
- Discriminated unions for all engine types (`GameAction`, `GameEvent`, `WaitingFor`) — view model maps these to flat props
- Zustand stores with selector pattern — components subscribe to specific slices
- Tailwind CSS v4 for styling — CSS custom properties integrate naturally
- Framer Motion for animations — available for hand expand/collapse, log slide-out

### Integration Points
- `EngineAdapter` interface: all UI reads state via `getState()` and dispatches via `submitAction()` — unchanged
- `GameState.objects`: Central `Record<string, GameObject>` — view model maps from this
- `Player.hand`, `Player.graveyard`: Zone arrays of ObjectIds — used for hand display and zone viewer counts
- `GameState.battlefield`: ObjectId array — view model groups by card type for three-row layout
- `WaitingFor` union: Drives overlay display (targeting, combat, mana payment) — existing pattern preserved

</code_context>

<deferred>
## Deferred Ideas

None — discussion stayed within phase scope

</deferred>

---

*Phase: 13-foundation-board-layout*
*Context gathered: 2026-03-08*
