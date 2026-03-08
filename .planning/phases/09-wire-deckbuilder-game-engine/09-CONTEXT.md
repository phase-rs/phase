# Phase 9: Wire DeckBuilder to Game Engine - Context

**Gathered:** 2026-03-08
**Status:** Ready for planning

<domain>
## Phase Boundary

Fix the DeckBuilder → Start Game → Play with Deck flow end-to-end. Deck data passes correctly from the deck management UI to the game engine, the game launches with the correct mode (AI/solo), and the WASM engine instantiates cards from the deck. Closes integration gaps DECK-01, DECK-03 and flows "DeckBuilder → Start Game → Play with Deck", "DeckBuilder → Start Game (mode)" from v1.0 audit.

</domain>

<decisions>
## Implementation Decisions

### Start Game Flow
- Remove "Start Game" button from DeckBuilder — DeckBuilder is purely for building/editing decks
- MenuPage becomes the game launch point with an MTGA-style deck selector
- Deck tiles displayed as horizontal scrollable row showing: deck name, color identity dots, card count
- Tap a tile to select it as the active deck; selected tile visually highlighted
- Active deck persists in localStorage across sessions
- Keep existing difficulty buttons (Easy/Medium/Hard/Expert) under "Play vs AI" section on MenuPage
- Deck tiles appear above the game mode buttons

### Starter Decks
- Hardcoded list of MTGA starter decks bundled in client as JSON data
- Available on first launch — no network fetch required for deck lists
- Card images load on demand via existing Scryfall pipeline
- Starter decks update with app releases (MTGA occasionally changes them)

### MTGA Text Import
- Add "Import" button in DeckBuilder that opens a paste modal/textarea
- Parse MTGA deck text format (`4 Lightning Bolt (FDN) 123`)
- Auto-detect format (MTGA text vs Forge .dck — existing .dck parser already works)
- Import creates/replaces the current deck in DeckBuilder

### Deck Data Format
- Compact format {name, count}[] passed from client to WASM (not expanded arrays)
- Client resolves card names to card definitions before passing to WASM
- WASM engine receives card definitions + counts, creates GameObjects internally
- WASM `initialize_game` expands counts into individual GameObjects in library zone
- sessionStorage key mismatch fixed (DeckBuilder writes "forge-active-deck", GamePage reads "forge-deck" — unify to one key)

### Missing Deck Handling
- MenuPage: if no saved decks exist, show "No decks yet" with [Build a Deck] and [Import] buttons
- Game mode buttons disabled when no active deck is selected
- GamePage: if navigated to /game without an active deck, redirect to MenuPage
- Starter decks ensure first-launch always has decks available

### Claude's Discretion
- Exact deck tile visual design (size, spacing, selected state indicator)
- MTGA text format parser implementation details
- How card definitions are resolved client-side (build-time pre-computation vs runtime)
- Starter deck list curation (which specific MTGA starter decks to include)
- sessionStorage vs localStorage for active deck data passed to GamePage

</decisions>

<specifics>
## Specific Ideas

- "Like MTGA's deck tray on the home screen" — deck selector should feel familiar to Arena players
- Deck tiles show color identity as colored dots (WUBRG pip style)
- First-launch experience should have decks ready to play immediately (starter decks)
- Import supports at minimum MTGA text format (`4 Card Name (SET) ###`) and existing .dck format

</specifics>

<code_context>
## Existing Code Insights

### Reusable Assets
- `DeckBuilder.tsx`: Already has save/load with localStorage `forge-deck:` prefix
- `deckParser.ts`: Existing .dck/.dec file parser — extend for MTGA text format
- `WasmAdapter.initializeGame(deckData?)`: Stubbed method ready for deck data
- `initialize_game(_deck_data: JsValue)` in engine-wasm: Accepts JsValue, currently ignores it
- `MenuPage.tsx`: Already has difficulty buttons and game mode navigation
- Scryfall image pipeline: CardImage, IndexedDB caching, rate-limited API client

### Established Patterns
- localStorage with `forge-deck:` key prefix for deck persistence
- sessionStorage for ephemeral game session data (deck handoff, WebSocket sessions)
- Zustand stores for reactive state (gameStore, uiStore)
- WASM bindings via wasm-bindgen with serde_wasm_bindgen serialization

### Integration Points
- `DeckBuilder.handleStartGame()` → remove, navigation moves to MenuPage
- `MenuPage` → add deck tile selector, wire active deck to game launch
- `GamePage.loadDeckFromSession()` → read active deck from localStorage (or sessionStorage)
- `gameStore.initGame()` → pass deck data to adapter
- `WasmAdapter.initialize()` → call `initialize_game` with deck data
- `engine-wasm::initialize_game` → deserialize deck, create GameObjects in library zones

### Key Bugs Found
- sessionStorage key mismatch: DeckBuilder writes `"forge-active-deck"`, GamePage reads `"forge-deck"`
- DeckBuilder navigates to `/game` without mode param — AI never starts
- `gameStore.initGame` accepts `_deckData` but never passes it to the adapter
- WASM `initialize_game` ignores `_deck_data` parameter entirely

</code_context>

<deferred>
## Deferred Ideas

- Aetherhub meta deck fetching — query popular/meta decks from Aetherhub API (future phase)
- Deck sharing/export to MTGA format
- Sideboard management during best-of-3 games (MODE-04)

</deferred>

---

*Phase: 09-wire-deckbuilder-game-engine*
*Context gathered: 2026-03-08*
