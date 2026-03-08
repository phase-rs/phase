# Phase 9: Wire DeckBuilder to Game Engine - Research

**Researched:** 2026-03-08
**Domain:** Client-WASM integration, deck data flow, MTGA deck format parsing
**Confidence:** HIGH

## Summary

This phase closes the DeckBuilder-to-GameEngine integration gap. The codebase has all the pieces (DeckBuilder UI, WASM engine, game stores, adapters) but they are not wired together: sessionStorage keys mismatch, deck data is never passed to WASM, and `initialize_game` ignores its input. The core work is: (1) fix the data flow from MenuPage deck selection through sessionStorage/localStorage to WASM, (2) build a `CardFace -> GameObject` hydration function in the engine since none exists, (3) add MTGA text format parsing, (4) add starter decks, and (5) restructure the menu flow so DeckBuilder is for building and MenuPage is the game launch point.

A critical constraint: `CardDatabase` requires filesystem access unavailable in WASM. The approach from CONTEXT.md -- client resolves card names to card definitions before passing to WASM -- means deck data sent to WASM must include full CardFace-equivalent data, not just card names. The WASM `initialize_game` then hydrates GameObjects from this resolved data.

**Primary recommendation:** Build a `game_object_from_card_data` function in the engine that populates all GameObject fields from a serializable card definition struct, then wire the full pipeline: MenuPage deck selector -> localStorage active deck -> GamePage reads deck -> client resolves card defs -> WASM `initialize_game` hydrates GameObjects into library zones.

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions
- Remove "Start Game" button from DeckBuilder -- DeckBuilder is purely for building/editing decks
- MenuPage becomes the game launch point with an MTGA-style deck selector
- Deck tiles displayed as horizontal scrollable row showing: deck name, color identity dots, card count
- Tap a tile to select it as the active deck; selected tile visually highlighted
- Active deck persists in localStorage across sessions
- Keep existing difficulty buttons (Easy/Medium/Hard/Expert) under "Play vs AI" section on MenuPage
- Deck tiles appear above the game mode buttons
- Hardcoded list of MTGA starter decks bundled in client as JSON data
- Available on first launch -- no network fetch required for deck lists
- Card images load on demand via existing Scryfall pipeline
- Starter decks update with app releases
- Add "Import" button in DeckBuilder that opens a paste modal/textarea
- Parse MTGA deck text format (`4 Lightning Bolt (FDN) 123`)
- Auto-detect format (MTGA text vs Forge .dck -- existing .dck parser already works)
- Import creates/replaces the current deck in DeckBuilder
- Compact format {name, count}[] passed from client to WASM (not expanded arrays)
- Client resolves card names to card definitions before passing to WASM
- WASM engine receives card definitions + counts, creates GameObjects internally
- WASM `initialize_game` expands counts into individual GameObjects in library zone
- sessionStorage key mismatch fixed (unify to one key)
- MenuPage: if no saved decks exist, show "No decks yet" with [Build a Deck] and [Import] buttons
- Game mode buttons disabled when no active deck is selected
- GamePage: if navigated to /game without an active deck, redirect to MenuPage

### Claude's Discretion
- Exact deck tile visual design (size, spacing, selected state indicator)
- MTGA text format parser implementation details
- How card definitions are resolved client-side (build-time pre-computation vs runtime)
- Starter deck list curation (which specific MTGA starter decks to include)
- sessionStorage vs localStorage for active deck data passed to GamePage

### Deferred Ideas (OUT OF SCOPE)
- Aetherhub meta deck fetching -- query popular/meta decks from Aetherhub API (future phase)
- Deck sharing/export to MTGA format
- Sideboard management during best-of-3 games (MODE-04)
</user_constraints>

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|-----------------|
| DECK-01 | Deck builder with card search and filtering | DeckBuilder already exists with CardSearch/CardGrid/DeckList; needs Import button, starter decks, and removal of Start Game button |
| DECK-03 | Mana curve and color distribution display | ManaCurve component already exists and works; ensure it continues working after deck data format changes |
| AI-04 | Game tree search (leveraging Rust native performance) | Already complete; this phase ensures deck data reaches the AI game via proper initialization |
| PLAT-03 | EngineAdapter abstraction (Tauri IPC and WASM bindings) | WasmAdapter.initializeGame() exists but never receives deck data; gameStore.initGame() accepts deckData but ignores it |
</phase_requirements>

## Standard Stack

### Core
| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| React | 19 | UI framework | Already in use |
| react-router | 7 | Client routing | Already in use, MenuPage/GamePage navigation |
| Zustand | 5 | State management | Already in use (gameStore, uiStore) |
| Tailwind CSS | v4 | Styling | Already in use, CSS-first config |
| wasm-bindgen | latest | WASM-JS bridge | Already in use for engine-wasm |
| serde_wasm_bindgen | latest | Rust<->JS serialization | Already in use for all WASM data |

### Supporting
| Library | Version | Purpose | When to Use |
|---------|---------|---------|-------------|
| Scryfall client | existing | Card image loading | Deck tile color identity, card previews |
| vitest | 3.x | Test framework | Unit tests for parsers, stores |

### Alternatives Considered
None -- this phase uses the existing stack entirely. No new dependencies needed.

## Architecture Patterns

### Data Flow: Deck Selection to Game Start
```
┌─────────────┐     localStorage      ┌───────────┐    localStorage     ┌──────────┐
│ DeckBuilder │ ──save deck──────────> │ MenuPage  │ ──active deck────> │ GamePage │
│ (build/edit)│ <──load deck───────── │ (launch)  │                    │ (play)   │
└─────────────┘   forge-deck:{name}   └───────────┘  forge-active-deck └──────────┘
                                           │                                │
                                     read all decks                   read active deck
                                     from forge-deck:*                resolve card defs
                                     select active →                  pass to WASM
                                     store in                              │
                                     forge-active-deck                     ▼
                                                                    ┌──────────────┐
                                                                    │ WASM Engine  │
                                                                    │ init_game()  │
                                                                    │ hydrate objs │
                                                                    │ shuffle libs │
                                                                    └──────────────┘
```

### Storage Key Convention
- `forge-deck:{name}` -- saved deck data (ParsedDeck JSON) in localStorage
- `forge-active-deck` -- currently selected deck name in localStorage (persists across sessions)
- **No sessionStorage needed** for deck handoff -- active deck name in localStorage, GamePage reads the deck data from `forge-deck:{activeName}`

### Pattern 1: Card Definition Resolution (Client-Side)
**What:** The client must resolve card names to card definitions before passing to WASM, since CardDatabase requires filesystem access unavailable in WASM.
**When to use:** Before calling `WasmAdapter.initializeGame()`
**Approach:** Build-time pre-computation is the established pattern (Phase 8 decision). A CLI tool generates a JSON file mapping card names to serializable card definitions. The client loads this JSON (or a subset) and resolves deck card names to full definitions.

Alternative (simpler for Phase 9): Pass card names + counts to WASM, and embed a minimal card data set directly in the WASM binary via `include_str!` or a build script. However, this increases WASM binary size.

**Recommended approach:** Client-side resolution via a pre-computed JSON file at `/card-data.json` (same pattern as `/coverage-data.json`). The CLI binary already has `CardDatabase::load()` access. Add a new binary that exports card definitions as JSON.

```typescript
// Client-side card resolution
interface CardDefinition {
  name: string;
  mana_cost: ManaCost;
  card_types: CardType;
  power?: number;
  toughness?: number;
  loyalty?: number;
  keywords: string[];
  abilities: string[];
  triggers: string[];
  static_abilities: string[];
  replacements: string[];
  svars: Record<string, string>;
  color?: string[];
}

interface DeckPayload {
  cards: Array<{ card: CardDefinition; count: number }>;
}
```

### Pattern 2: GameObject Hydration in WASM
**What:** A Rust function that creates a fully-populated GameObject from a card definition, setting all fields the engine needs (card_types, power, toughness, keywords with `parse_keywords`, abilities, trigger_definitions, replacement_definitions, static_definitions, svars, color, mana_cost).
**When to use:** In `initialize_game` when processing deck data.

```rust
// In engine crate (not engine-wasm, so it's testable)
pub fn create_object_from_card(
    state: &mut GameState,
    card: &CardFace,
    owner: PlayerId,
) -> ObjectId {
    let id = zones::create_object(
        state, CardId(state.next_object_id), owner,
        card.name.clone(), Zone::Library,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types = card.card_type.clone();
    obj.mana_cost = card.mana_cost.clone();
    obj.power = card.power.as_ref().and_then(|p| p.parse().ok());
    obj.toughness = card.toughness.as_ref().and_then(|t| t.parse().ok());
    obj.base_power = obj.power;
    obj.base_toughness = obj.toughness;
    obj.keywords = parse_keywords(&card.keywords);
    obj.base_keywords = obj.keywords.clone();
    obj.abilities = card.abilities.clone();
    obj.svars = card.svars.clone();
    // Parse trigger/replacement/static definitions from strings
    // ... (using existing ability parser infrastructure)
    obj.color = card.color_override.clone()
        .unwrap_or_else(|| derive_color_from_mana_cost(&card.mana_cost));
    obj.base_color = obj.color.clone();
    id
}
```

### Pattern 3: MTGA Text Format Parsing
**What:** Parser for MTGA deck export format
**Format:** `4 Lightning Bolt (FDN) 123` -- count, card name, (set code), collector number
**Detection:** Lines contain parenthesized set codes `(XXX)` with trailing numbers

```typescript
// MTGA format: "4 Lightning Bolt (FDN) 123"
const MTGA_LINE_RE = /^(\d+)\s+(.+?)\s+\([A-Z0-9]+\)\s+\d+$/;

function parseMtgaDeck(content: string): ParsedDeck {
  const lines = content.split(/\r?\n/);
  const deck: ParsedDeck = { main: [], sideboard: [] };
  let section: 'main' | 'sideboard' = 'main';

  for (const raw of lines) {
    const line = raw.trim();
    if (!line) { section = 'sideboard'; continue; } // blank line separates main/side
    const match = line.match(MTGA_LINE_RE);
    if (match) {
      deck[section].push({ count: parseInt(match[1]), name: match[2] });
    }
  }
  return deck;
}
```

### Pattern 4: Starter Deck Data
**What:** Bundled JSON array of starter decks available on first launch
**Location:** `client/src/data/starter-decks.ts` as a typed constant

```typescript
interface StarterDeck {
  name: string;
  colorIdentity: string[]; // ["W", "U"] etc.
  cards: DeckEntry[];
}

export const STARTER_DECKS: StarterDeck[] = [
  {
    name: "Azorius Control",
    colorIdentity: ["W", "U"],
    cards: [
      { count: 4, name: "Counterspell" },
      // ...
    ],
  },
  // ...
];
```

### Anti-Patterns to Avoid
- **Passing card names only to WASM:** WASM cannot look up cards from filesystem. Must pass resolved card definitions.
- **Expanding counts before serialization:** Don't send 60 individual card objects across the WASM boundary. Send `{card, count}[]` and expand inside Rust.
- **Using sessionStorage for deck persistence:** Active deck selection should survive page refreshes (localStorage). sessionStorage is only for ephemeral data like WebSocket sessions.

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| Card data resolution | Custom runtime card lookup | Pre-computed JSON from CLI binary | CardDatabase needs filesystem, matches Phase 8 pattern |
| Keyword parsing | Manual string matching | `parse_keywords()` in engine | Already handles 50+ keywords with parameterized variants |
| Trigger/static/replacement parsing | Manual definition creation | Existing ability parser infrastructure | `parse_ability`, `TriggerDefinition::from`, etc. |
| Deck format detection | Complex heuristic | Simple regex check for `(SET) NUM` pattern | MTGA format has distinctive `(XXX) ###` suffix |
| Scrollable deck tiles | Custom scroll container | Tailwind `overflow-x-auto flex gap-*` | Standard CSS horizontal scroll |

## Common Pitfalls

### Pitfall 1: sessionStorage Key Mismatch (KNOWN BUG)
**What goes wrong:** DeckBuilder writes `"forge-active-deck"` to sessionStorage, GamePage reads `"forge-deck"` -- game starts with no deck.
**Why it happens:** Keys were defined independently in different files.
**How to avoid:** Define storage keys as shared constants imported by all consumers.
**Warning signs:** Game starts with empty libraries despite having selected a deck.

### Pitfall 2: WASM Binary Size Bloat from Embedded Card Data
**What goes wrong:** Embedding full card database in WASM binary increases download from 19KB to potentially megabytes.
**Why it happens:** Tempting to use `include_str!` for card definitions.
**How to avoid:** Keep card data as a separate JSON file loaded by the client. Pass resolved data to WASM per-game-session.
**Warning signs:** WASM binary size exceeds 1MB.

### Pitfall 3: Missing GameObject Field Population
**What goes wrong:** Objects created in library zone have empty keywords/abilities/triggers, so spells don't work when cast.
**Why it happens:** `create_object` only sets name, id, owner, zone. All card characteristics must be set separately.
**How to avoid:** The hydration function must populate ALL fields: card_types, mana_cost, power/toughness (parsed from string to i32), keywords (via `parse_keywords`), abilities, trigger_definitions (parsed from trigger strings), replacement_definitions, static_definitions, svars, color (derived or overridden).
**Warning signs:** Cards can be drawn but can't be cast or have no effects when resolved.

### Pitfall 4: Missing Library Shuffle After Deck Loading
**What goes wrong:** Library order is deterministic based on deck list order -- not randomized.
**Why it happens:** `create_object` with `Zone::Library` appends to library in insertion order.
**How to avoid:** After populating all objects, shuffle each player's library using `state.rng`.
**Warning signs:** Every game starts with the same draw order.

### Pitfall 5: Power/Toughness String-to-Number Parsing
**What goes wrong:** CardFace stores power/toughness as `Option<String>` (e.g., "2", "*", "1+*"). GameObject stores them as `Option<i32>`.
**Why it happens:** MTG has variable P/T values.
**How to avoid:** Parse numeric values with fallback. "*" values can default to 0 (the engine handles characteristic-defining abilities via the layer system).
**Warning signs:** Creatures appear with 0/0 stats.

### Pitfall 6: GamePage Redirect Without Active Deck
**What goes wrong:** Navigating directly to `/game` (e.g., bookmark) with no active deck crashes.
**Why it happens:** No guard check before initializing game.
**How to avoid:** Check for active deck in GamePage useEffect. If missing, `navigate("/")` immediately.
**Warning signs:** Blank game screen or WASM errors in console.

## Code Examples

### Existing: DeckBuilder Save (localStorage)
```typescript
// Source: client/src/components/deck-builder/DeckBuilder.tsx
const STORAGE_KEY_PREFIX = "forge-deck:";
localStorage.setItem(STORAGE_KEY_PREFIX + deckName.trim(), data);
```

### Existing: GamePage Deck Load (BROKEN)
```typescript
// Source: client/src/pages/GamePage.tsx
// BUG: reads "forge-deck" but DeckBuilder writes "forge-active-deck"
function loadDeckFromSession(): DeckData {
  const raw = sessionStorage.getItem("forge-deck");
  // ...
}
```

### Existing: WasmAdapter.initializeGame (UNUSED)
```typescript
// Source: client/src/adapter/wasm-adapter.ts
initializeGame(deckData?: unknown): GameEvent[] {
  this.assertInitialized();
  const result = initialize_game(deckData ?? null);
  return result.events ?? [];
}
```

### Existing: gameStore.initGame (IGNORES DECK DATA)
```typescript
// Source: client/src/stores/gameStore.ts
initGame: async (adapter, _deckData) => {
  await adapter.initialize();
  const state = await adapter.getState();
  // _deckData is never used
  set({ adapter, gameState: state, ... });
},
```

### Existing: WASM initialize_game (IGNORES DECK DATA)
```rust
// Source: crates/engine-wasm/src/lib.rs
pub fn initialize_game(_deck_data: JsValue) -> JsValue {
  let state = GameState::new_two_player(42); // ignores _deck_data
  // ...
}
```

### Existing: zones::create_object (USED FOR HYDRATION)
```rust
// Source: crates/engine/src/game/zones.rs
pub fn create_object(
    state: &mut GameState,
    card_id: CardId,
    owner: PlayerId,
    name: String,
    zone: Zone,
) -> ObjectId {
    let id = ObjectId(state.next_object_id);
    state.next_object_id += 1;
    let obj = GameObject::new(id, card_id, owner, name, zone);
    state.objects.insert(id, obj);
    add_to_zone(state, id, zone, owner);
    id
}
```

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| DeckBuilder has Start Game button | MenuPage is game launch point | Phase 9 decision | DeckBuilder becomes pure editor |
| sessionStorage for deck handoff | localStorage for active deck persistence | Phase 9 decision | Deck selection survives refresh |
| Game starts with empty state | Game starts with deck-loaded libraries | Phase 9 | Full gameplay loop enabled |

## Open Questions

1. **Card Definition Resolution Strategy**
   - What we know: CardDatabase needs filesystem; Phase 8 established build-time pre-computation pattern; coverage-report binary exists as precedent
   - What's unclear: Should we build a `card-data-export` binary now, or can we use a simpler approach for Phase 9 (e.g., pass CardFace-equivalent data from client, where the client constructs it from Scryfall API data already cached in DeckBuilder)?
   - Recommendation: For Phase 9, use the simplest viable path. The DeckBuilder already caches `ScryfallCard` data. Map ScryfallCard fields to a `CardDefinition` struct that WASM can consume. Full Forge card definitions (with ability strings, SVars, triggers) require the build-time export -- but for MVP deck loading, basic card data (name, mana_cost, types, P/T, keywords) enables gameplay. Ability parsing from Forge .txt files is a separate concern (cards already work via the engine's existing test infrastructure that manually sets up objects). **Simplest approach: send card names to WASM, add a minimal embedded card lookup in WASM for the starter deck cards, or send full CardFace JSON from a pre-computed export.**

2. **Which Starter Decks to Include**
   - What we know: MTGA starter decks, should be playable immediately
   - What's unclear: Exact list -- needs to be curated from cards the engine actually supports
   - Recommendation: Cross-reference starter deck cards against the card coverage data. Pick 3-5 mono/dual-color decks where 90%+ of cards are supported.

3. **Trigger/Replacement/Static Parsing in Hydration**
   - What we know: CardFace stores triggers/statics/replacements as `Vec<String>`. Engine tests manually construct `TriggerDefinition` structs.
   - What's unclear: Is there a public API to parse trigger strings into TriggerDefinition? The trigger module has matching logic but parsing from raw strings may need a new function.
   - Recommendation: For Phase 9, prioritize basic card loading (creatures with P/T, instants/sorceries with abilities). Complex trigger/static/replacement parsing can use existing `parse_ability` infrastructure.

## Validation Architecture

### Test Framework
| Property | Value |
|----------|-------|
| Framework | vitest 3.x (client), cargo test (Rust) |
| Config file | `client/vitest.config.ts` |
| Quick run command | `cd client && npx vitest run --reporter=verbose` |
| Full suite command | `cd client && npx vitest run && cd ../crates && cargo test` |

### Phase Requirements -> Test Map
| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| DECK-01 | MTGA text format parser | unit | `cd client && npx vitest run src/services/__tests__/deckParser.test.ts -x` | Exists but needs MTGA tests |
| DECK-01 | Starter decks load on first launch | unit | `cd client && npx vitest run src/data/__tests__/starterDecks.test.ts -x` | Wave 0 |
| DECK-03 | ManaCurve renders with deck data | unit | `cd client && npx vitest run src/components/deck-builder/__tests__/ManaCurve.test.ts -x` | Wave 0 |
| AI-04 | Game initializes with deck data passed to WASM | integration | `cd crates && cargo test -p engine-wasm -- initialize_game` | Wave 0 |
| PLAT-03 | gameStore.initGame passes deckData to adapter | unit | `cd client && npx vitest run src/stores/__tests__/gameStore.test.ts -x` | Exists but needs deck data tests |
| PLAT-03 | Storage key constants are shared | unit | `cd client && npx vitest run src/constants/__tests__/storage.test.ts -x` | Wave 0 |

### Sampling Rate
- **Per task commit:** `cd client && npx vitest run --reporter=verbose`
- **Per wave merge:** `cd client && npx vitest run && cd ../crates/engine && cargo test && cd ../engine-wasm && cargo test`
- **Phase gate:** Full suite green before `/gsd:verify-work`

### Wave 0 Gaps
- [ ] `client/src/services/__tests__/deckParser.test.ts` -- add MTGA format parse tests
- [ ] `client/src/data/__tests__/starterDecks.test.ts` -- validate starter deck structure
- [ ] `crates/engine/src/game/deck_loading.rs` + tests -- GameObject hydration from card data
- [ ] `crates/engine-wasm/src/lib.rs` tests -- initialize_game with deck data

## Sources

### Primary (HIGH confidence)
- Direct codebase inspection of all integration points:
  - `client/src/components/deck-builder/DeckBuilder.tsx` -- storage keys, save/load, Start Game flow
  - `client/src/pages/MenuPage.tsx` -- current menu structure, difficulty buttons
  - `client/src/pages/GamePage.tsx` -- loadDeckFromSession bug, adapter initialization
  - `client/src/stores/gameStore.ts` -- initGame signature, unused deckData parameter
  - `client/src/adapter/wasm-adapter.ts` -- initializeGame method
  - `client/src/adapter/types.ts` -- EngineAdapter interface, GameObject type
  - `client/src/services/deckParser.ts` -- existing .dck parser
  - `crates/engine-wasm/src/lib.rs` -- initialize_game ignoring deck_data
  - `crates/engine/src/game/zones.rs` -- create_object function
  - `crates/engine/src/game/game_object.rs` -- GameObject struct and fields
  - `crates/engine/src/types/card.rs` -- CardFace, CardRules structures
  - `crates/engine/src/database/card_db.rs` -- CardDatabase filesystem dependency
  - `crates/engine/src/game/engine.rs` -- start_game, library detection for mulligan

### Secondary (MEDIUM confidence)
- MTGA deck format is well-documented: `count name (SET) collector_number`, blank line separates main from sideboard

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH - all libraries already in use, no new deps
- Architecture: HIGH - data flow fully traced through codebase, bugs identified
- Pitfalls: HIGH - bugs confirmed via direct code inspection (key mismatch, unused params)
- Card resolution strategy: MEDIUM - multiple viable approaches, exact implementation TBD

**Research date:** 2026-03-08
**Valid until:** 2026-04-08 (stable domain, internal integration work)
