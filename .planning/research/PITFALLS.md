# Domain Pitfalls: Arena UI Port (Alchemy to Forge.rs)

**Domain:** Porting a card game UI from a simpler engine to a complex one
**Researched:** 2026-03-08
**Source projects:** Alchemy (simple: 5 elements, fixed board slots, sync TS engine) -> Forge.rs (complex: MTG 5 colors, unlimited permanents, async WASM engine, stack/priority)

---

## Critical Pitfalls

Mistakes that cause rewrites or major issues when porting UI between engines with different complexity levels.

---

### Pitfall 1: Fixed Board Slots vs. Dynamic Permanent Count

**What goes wrong:** Alchemy's `PlayerState.board` is `(Permanent | null)[]` with a fixed `maxBoardSize` (typically 5 slots per player). The `CreatureSlots` component is designed around this bounded array -- it calculates card sizes against a known maximum, uses slot indices for creature placement (`targetSlot` in `PLAY_CARD`), and the `boardStacking` and `boardSizing` modules assume a bounded upper limit. Forge.rs's battlefield is `ObjectId[]` with no upper bound, partitioned by card type (creatures, lands, other permanents). A single player can control 20+ permanents of different types simultaneously.

**Why it happens:** Developers port the `CreatureSlots` component and wire it to `gameState.battlefield`, but the layout breaks when a token deck creates 15 creatures, or when lands (which Alchemy doesn't have as permanents) need their own row. The `calculateBoardCardSize` function works well for 5-7 cards but produces unusable sizes at 15+.

**Consequences:** Cards shrink to unreadable sizes. Board becomes unscrollable. Layout overflow causes z-index layering bugs. Lands and enchantments have nowhere to display. Touch targets become too small on mobile.

**Prevention:**
- Replace the single `CreatureSlots` component with Forge.rs's existing `GameBoard` pattern: separate `BattlefieldRow` components for creatures, lands, and other permanents, each independently scrollable.
- Port the `groupIntoStacks` logic but parameterize it for unbounded counts -- the stacking threshold in Alchemy (lines 400-407 of `CreatureSlots.tsx`) uses `minWidth = baseWidth * 0.7` which is already decent, but needs a fallback for 15+ cards (e.g., horizontal scroll or multi-row grid).
- Keep Alchemy's `CardStackGroup` concept but add aggressive stacking by card name for tokens (10 identical 1/1 Soldier tokens should stack to a single visual with a count badge, not 10 individual cards).
- The Forge.rs `BattlefieldRow` already uses `flex-wrap` -- preserve this. Alchemy's `CreatureSlots` uses `w-fit mx-auto` single-row flex with horizontal scroll, which is fine for 5 cards but poor for 15.

**Detection:** Test with token-heavy decks (e.g., a deck that creates 10+ creature tokens). If cards are unreadable or overflow, the layout assumptions are wrong.

**Confidence:** HIGH -- directly observable from code comparison. Alchemy's `RulesetConfig.maxBoardSize` is 5; Forge.rs has no equivalent limit.

**Phase:** Phase 1 (Board Layout Port). Must be resolved before any creature rendering.

---

### Pitfall 2: Synchronous Dispatch vs. Async WASM Boundary

**What goes wrong:** Alchemy's `gameStore.dispatch()` is synchronous: it calls `reduce(state, action, actingPlayer, rng)` inline, gets back `{ newState, events }`, and immediately updates the store with `legalActions`. There is zero async gap. Alchemy's animation system relies on this -- `groupEventsIntoSteps` receives events synchronously in the same call stack, and `boardSnapshot` is set before dispatch to preserve dying creatures.

Forge.rs's `gameStore.dispatch()` is `async`: it calls `adapter.submitAction(action)` which goes through a promise queue (`WasmAdapter.enqueue`), then calls `adapter.getState()` (another async call). There is a multi-frame gap between dispatching an action and receiving the new state.

**Why it happens:** Alchemy's animation pipeline assumes: (1) capture board snapshot, (2) dispatch action synchronously, (3) immediately receive events, (4) call `groupEventsIntoSteps` with events, (5) enqueue animation steps. In Forge.rs, steps 2-3 are async. If the animation system tries to capture a snapshot and then dispatch, the snapshot may be stale by the time events arrive (other React renders may have occurred).

**Consequences:** Animations show stale positions. "Dying creature" snapshots expire before death animations play. Combat math bubbles appear at wrong positions. Floating damage numbers target elements that have already unmounted.

**Prevention:**
- Forge.rs already has a `dispatchWithAnimations` pattern (via the animation store). Port Alchemy's `groupEventsIntoSteps` but ensure it runs *after* the async dispatch resolves, not synchronously alongside it.
- The `boardSnapshot` pattern from Alchemy is correct in concept but must be adapted: capture the snapshot *before* the async dispatch, store it in the animation store, and clear it only when the death animation step begins (Alchemy already does this in `advanceStep` line 156).
- The position registry (`registerPosition`/`unregisterPosition`) is portable as-is -- it's a mutable module-level Map, not tied to React render timing. But the `getPositions()` call must happen at animation-enqueue time (after async dispatch resolves), not at dispatch-start time.
- Wrap the entire dispatch-animate flow in a serialized queue: `await dispatch(action) -> events -> groupEventsIntoSteps(events, positions) -> enqueueSteps(steps)`. No concurrent dispatches during animation playback.

**Detection:** Play a combat round. If damage numbers appear at coordinates (0,0) or at the card's pre-combat position instead of post-attack position, position capture timing is wrong. If dying creatures disappear before their death particle animation, the snapshot timing is wrong.

**Confidence:** HIGH -- directly observable from comparing `gameStore.dispatch` in both projects. Alchemy line 171: `const result = reduce(state, action, actingPlayer, rng)` (sync). Forge.rs line 57: `const events = await adapter.submitAction(action)` (async).

**Phase:** Phase 2 (Animation System Port). The most architecturally sensitive port.

---

### Pitfall 3: Type Shape Mismatch -- Alchemy's Flat Model vs. Forge.rs's Object Graph

**What goes wrong:** Alchemy's `Permanent` is a flat struct with inline stats: `{ permanentId, cardId, ownerId, attack, health, damage, isTapped, summonedThisTurn, temporaryAttackBonus, temporaryHealthBonus }`. Stats are directly on the object. Card data is looked up via `CARD_REGISTRY[cardId]`.

Forge.rs's `GameObject` is a deep object from the WASM boundary: `{ id, card_id, owner, controller, zone, tapped, power, toughness, damage_marked, card_types, mana_cost, keywords, counters, attachments, color, ... }`. There is no separate "card registry" in the frontend -- all card data comes from the engine state. Power/toughness are engine-computed (layer system applied). There is no `attack`/`health` -- the fields are `power`/`toughness`.

**Why it happens:** Developers copy Alchemy components and try to map props 1:1. `permanent.attack` becomes `gameObject.power`, `permanent.health` becomes `gameObject.toughness`. But these are semantically different: Alchemy's `attack` is base + temporary bonus; Forge.rs's `power` is already layer-evaluated (includes +1/+1 counters, enchantment buffs, type-changing effects, etc.). Applying Alchemy's buff detection logic (`isBuffedAttack = attack > baseAttack`) to Forge.rs data produces wrong visual indicators because `base_power` vs `power` comparison doesn't account for counter-based buffs vs spell-based buffs.

**Consequences:** Buff indicators wrong (card shows "buffed" when it has counters, which are permanent not temporary). Health/toughness display wrong (shows toughness where it should show toughness minus damage). Card type display wrong (Alchemy has `creature | spell`; MTG has creatures, instants, sorceries, enchantments, artifacts, planeswalkers, lands, each needing different visual treatment).

**Prevention:**
- Create a mapping layer (adapter component or hook) that converts Forge.rs's `GameObject` into a view model matching Alchemy's `BoardCard` expectations. Do NOT spread `GameObject` props directly into Alchemy components.
- The view model should compute:
  - `effectiveAttack` = `gameObject.power` (already layer-evaluated)
  - `effectiveHealth` = `gameObject.toughness - gameObject.damage_marked`
  - `isBuffed` = `gameObject.power > gameObject.base_power || gameObject.toughness > gameObject.base_toughness`
  - `isDamaged` = `gameObject.damage_marked > 0`
  - `isSummoningSick` = `gameObject.entered_battlefield_turn === currentTurn && !gameObject.keywords.includes("Haste")`
  - `cardType` = derived from `gameObject.card_types.core_types` (not a simple string enum)
- The view model replaces Alchemy's `CARD_REGISTRY` lookups -- Forge.rs cards don't have a static registry in the frontend.

**Detection:** Play a card with +1/+1 counters. If the UI shows it as "temporarily buffed" (with Alchemy's gold buff particles), the mapping is wrong. Play an enchantment -- if the UI doesn't know where to render it, the type mapping is incomplete.

**Confidence:** HIGH -- directly from comparing `Permanent` (Alchemy types.ts:57-70) with `GameObject` (Forge.rs adapter/types.ts:75-109).

**Phase:** Phase 1 (Component Adapter Layer). Must exist before any Alchemy component renders Forge.rs data.

---

### Pitfall 4: Missing Stack/Priority/Instant UI Concepts

**What goes wrong:** Alchemy has no stack visualization. Its phase system is linear: `mulligan -> draw -> energy -> play -> battle -> end`. The only instant-speed interaction is a limited `combat_priority` phase. Forge.rs has a full priority system where either player can respond at almost any point with instants and activated abilities. The stack (`StackEntry[]`) can have multiple items, each with their own targets and controllers. There is no Alchemy component for: stack visualization, mana payment modal, priority pass controls, or instant-speed card casting.

**Why it happens:** Developers port Alchemy's `ActionButton` (which handles `ADVANCE_PHASE`, `CONFIRM_ATTACKERS`, `CONFIRM_BLOCKERS`, `PASS_PRIORITY`) but don't realize Forge.rs's `PassPriority` happens dozens of times per turn. Alchemy's button toggles between 3-4 states; Forge.rs's priority system needs context-aware prompts ("Pass priority (opponent has mana open)", "Respond to Lightning Bolt targeting your creature?").

**Consequences:** Players cannot interact at instant speed. No way to see what's on the stack. No way to respond to spells. The game auto-passes priority without the player understanding what happened. Mana payment is impossible (Alchemy uses single-element costs; MTG uses multi-color, hybrid, and generic costs requiring interactive payment).

**Prevention:**
- These are NOT ports from Alchemy -- they are new components built from scratch for Forge.rs:
  - **Stack display:** Vertical list showing pending spells/abilities with source, controller, targets. Clickable to inspect.
  - **Priority indicator:** Shows whose priority it is, whether the active player can respond, and a "Pass" button that's always visible during priority windows.
  - **Mana payment modal:** Shows available mana, required cost, auto-pay suggestion with manual override. Triggered when `WaitingFor.type === "ManaPayment"`.
  - **Instant-speed casting:** Hand cards must be playable during opponent's turn when the player has priority. Alchemy's hand only allows plays during the `play` phase.
- Port Alchemy's `ActionButton` as the *base* but extend it significantly. The `WaitingFor` discriminated union in Forge.rs has 8 variants vs Alchemy's simpler phase model.
- Do NOT try to hide priority complexity. MTG players expect to see it. Auto-pass priority for phases where the player has no legal actions (Alchemy's approach of skipping irrelevant phases is correct, but must not skip phases where the player COULD respond).

**Detection:** Cast an instant during the opponent's combat phase. If the UI doesn't allow it, instant-speed interaction is broken. Check for stack display when multiple spells are pending.

**Confidence:** HIGH -- fundamental game mechanic difference. Alchemy has no equivalent UI to port.

**Phase:** Phase 3 (MTG-Specific UI). After board and animation ports are stable.

---

### Pitfall 5: Card Image Loading Model Change (Static Assets vs. Async API)

**What goes wrong:** Alchemy uses static assets: `getCardArtPath(cardId, element)` returns a synchronous path like `/cards/fire/flame_elemental.webp`. The `CardFace` component uses this as a CSS `background-image` with zero loading state -- the art either exists at that path or an element icon placeholder is shown. There is no loading spinner, no error handling for network failures, no rate limiting.

Forge.rs uses `useCardImage(cardName)` which is fully async: checks IndexedDB cache, then fetches from Scryfall API with 75ms rate limiting, returns `{ src: string | null, isLoading: boolean }`. A hand of 7 cards generates up to 7 sequential API calls on first load.

**Why it happens:** Alchemy's `CardFace` component treats art as an optional decoration -- `getCardArtPath` returns a path unconditionally and the element icon placeholder is always visible behind the art. If the art file doesn't exist (missing asset), the card is still fully readable. Port this to Forge.rs and every card shows the placeholder because there are no static art files -- all art comes from Scryfall.

**Consequences:** All cards show placeholder art until images load (can take 500ms-5s for uncached cards). No loading indicators. Broken or missing images show element icon placeholders that make no sense for MTG (there are no "elements" in MTG). Rate limiting causes cards loaded later in the hand to appear blank for seconds.

**Prevention:**
- Replace `getCardArtPath` calls with `useCardImage(cardName)` hook from Forge.rs, but add a loading skeleton to Alchemy's `CardFace` component:
  - Show card frame, name, type, stats immediately (these come from engine state, no network needed).
  - Show a shimmer/skeleton in the art area while `isLoading === true`.
  - Show the actual card image when `src` resolves.
- Replace element-based placeholders with generic card back or mana-color-based gradient placeholder. MTG colors (WUBRG) map naturally to gradient backgrounds.
- Batch-prefetch images: when game initializes, call `prefetchDeckImages` (already exists in Forge.rs scryfall.ts) for both players' decks. This front-loads the Scryfall calls before the board renders.
- For the card preview modal (`CardPreview`), use `size: "large"` for the detailed view and `size: "normal"` for board/hand cards.
- Consider using Scryfall's `image_uris.art_crop` for Alchemy-style art-only display (no card frame), keeping Alchemy's custom card frame rendering.

**Detection:** Start a game with no cached images. If all 7 hand cards and the initial battlefield are blank for >1 second, the loading UX needs work.

**Confidence:** HIGH -- directly from code comparison. Alchemy `cardUtils.ts:6-8` (sync path) vs Forge.rs `useCardImage.ts` (async with cache).

**Phase:** Phase 1 (Card Rendering). Must be resolved alongside the component adapter layer.

---

## Moderate Pitfalls

---

### Pitfall 6: Event System Shape Mismatch

**What goes wrong:** Alchemy's `GameEvent` and Forge.rs's `GameEvent` are both discriminated unions with `type` tags, but they have different shapes, different event names, and different data payloads. Alchemy's animation system (`groupEventsIntoSteps`) is tightly coupled to Alchemy's event shapes: it checks for `CREATURE_ENTERED`, `SPELL_RESOLVED`, `DAMAGE_DEALT`, `CREATURE_DIED` etc. Forge.rs events use different names: `ZoneChanged`, `SpellCast`, `DamageDealt`, `CreatureDestroyed`.

**Prevention:**
- Create an event mapping layer that converts Forge.rs events to Alchemy-compatible event shapes before passing to `groupEventsIntoSteps`. This is preferable to rewriting the animation grouping logic.
- Key mappings:
  - `ZoneChanged { to: "Battlefield" }` -> `CREATURE_ENTERED` (when object is a creature)
  - `DamageDealt` -> `DAMAGE_DEALT` (rename, restructure fields: `target: TargetRef` needs unpacking to `targetId: string`)
  - `LifeChanged { amount < 0 }` -> `PLAYER_DAMAGED`
  - `CreatureDestroyed` -> `CREATURE_DIED`
  - `SpellCast` -> `CARD_PLAYED` + `SPELL_RESOLVED` (Forge.rs separates cast and resolution; Alchemy combines them)
- Some Forge.rs events have no Alchemy equivalent and need new animation handling: `PermanentTapped`, `CounterAdded`, `TokenCreated`, `SpellCountered`, `ReplacementApplied`.

**Detection:** Cast a spell. If no animation plays, the event mapping is missing or wrong.

**Confidence:** HIGH -- directly from comparing event union types in both adapter/types.ts files.

**Phase:** Phase 2 (Animation System). The event mapper must exist before `groupEventsIntoSteps` can process Forge.rs events.

---

### Pitfall 7: Phase/Turn Model Impedance Mismatch

**What goes wrong:** Alchemy's `Phase` is a rich discriminated union carrying state (e.g., `{ type: 'battle', step: 'declare_attackers', tentativeAttackers: string[] }` embeds the list of tentative attackers directly in the phase object). Components read combat state directly from the phase. Forge.rs's `Phase` is a simple string enum (`"DeclareAttackers" | "DeclareBlockers" | ...`) and combat state lives in a separate `CombatState` object on the `GameState`. The `WaitingFor` discriminated union replaces Alchemy's phase as the primary "what should the UI show right now?" signal.

**Why it happens:** Alchemy's `CreatureSlots` reads `phase.tentativeAttackers`, `phase.confirmedAttackers`, `phase.blockers` directly from the phase. These fields don't exist in Forge.rs -- attackers are in `gameState.combat.attackers`, blockers are in `gameState.combat.blocker_assignments`.

**Consequences:** All combat UI code that reads from `phase` breaks. Blocker assignment, attacker declaration, combat resolution displays all fail.

**Prevention:**
- Create a `useCombatState()` hook that normalizes combat data from either source:
  - Reads `gameState.combat` (Forge.rs) or `phase.tentativeAttackers`/`phase.confirmedAttackers` (Alchemy).
  - Returns a consistent shape: `{ attackers, blockers, blockerToAttacker, step }`.
- Replace all `phase.type === 'battle' && phase.step === 'declare_attackers'` checks with `waitingFor.type === 'DeclareAttackers'` checks.
- The `shouldFanOut` function in `CreatureSlots` needs complete rewriting -- it checks Alchemy-specific phase shapes.
- Alchemy's `ActionButton` component determines button label and action from phase type. Replace with a `WaitingFor`-driven approach.

**Detection:** Enter combat. If the UI doesn't show attacker selection or blocker assignment overlays, the phase/combat state mapping is wrong.

**Confidence:** HIGH -- structurally incompatible phase models visible in both type files.

**Phase:** Phase 1 (Board Layout). Combat interaction is core to the board.

---

### Pitfall 8: WASM Serialization Artifacts (Map -> Record Conversion)

**What goes wrong:** Forge.rs's `WasmAdapter.fetchState()` includes a `convertMapsToRecords` function (wasm-adapter.ts:17-36) because `serde_wasm_bindgen` serializes Rust `HashMap<NonStringKey, V>` as JavaScript `Map`, not as plain objects. The frontend expects bracket access (`state.objects[id]`). If Alchemy components are ported and receive a `Map` instead of a `Record`, property access silently returns `undefined`.

**Prevention:**
- The `convertMapsToRecords` function already exists and works. Ensure it runs before any component receives state data.
- However, be aware that this recursive conversion is O(n) in state size. For large game states (30+ permanents), this adds measurable overhead on every `getState()` call. If animations are reading state frequently, this compounds.
- Consider moving hot-path data access to typed getter functions on the WASM side (`get_object(id) -> GameObject`) rather than serializing the entire `objects` map on every state fetch.

**Detection:** If a component reads `gameState.objects[someId]` and gets `undefined` despite the object existing, `convertMapsToRecords` may not have run, or a nested Map was missed.

**Confidence:** HIGH -- the `convertMapsToRecords` function in the codebase exists specifically because this problem was already encountered.

**Phase:** Phase 1 (Adapter Integration). Existing mitigation, but new components must be aware.

---

### Pitfall 9: Alchemy-Specific Concepts That Don't Map to MTG

**What goes wrong:** Alchemy components reference concepts that don't exist in MTG and will cause errors or nonsensical UI if ported without removal:
- **Elements** (`fire | water | earth | air | shadow`) -- used for card frame colors, art paths, particle effects, ambient music. MTG has **mana colors** (WUBRG + colorless) which serve a similar visual role but have different values and meanings.
- **Energy system** (`currentEnergy`, `maxEnergy`, `energyCap`) -- Alchemy uses a Hearthstone-style mana crystal system. MTG uses land-based mana with a pool that empties each phase.
- **Learning challenges** -- Alchemy's educational overlay system. Entirely absent from Forge.rs.
- **Tier system** (`apprentice | alchemist | archmage`) -- Alchemy's difficulty tiers. No MTG equivalent.
- **Creature types** (`angel | beast | dragon | ...`) -- Alchemy has a fixed enum of 12 types. MTG has 200+ creature subtypes as strings.
- **Spell speed** (`sorcery | instant`) -- Similar concept but Alchemy treats it as a card property; MTG treats it as a card type.

**Prevention:**
- Before porting any component, grep for Alchemy-specific imports: `@engine/types`, `@engine/cards`, `@engine/effects`, `@engine/keywords`. Every one of these needs replacement.
- Create a color mapping: `Element -> ManaColor[]`. A mono-red MTG card uses similar visuals to Alchemy's `fire`. Multi-color cards need blended gradients.
- Remove all `LearningChallengeOverlay`, `AdaptiveLearningToast`, `CoachOverlay`, `TutorialHelpPanel` references. These are Alchemy-only.
- Remove `HeroHUD` energy display. Replace with mana pool display from Forge.rs.
- The `getElementColor`, `getElementArtGradient`, `getElementFrameGradient`, `getElementIconPath` utility functions in Alchemy's `cardUtils.ts` need MTG equivalents: `getManaColorGradient(colors: ManaColor[])`, handling mono, dual, tri, and 5-color cards.

**Detection:** If the UI references "energy" or shows element icons, Alchemy concepts haven't been fully replaced.

**Confidence:** HIGH -- directly enumerable from Alchemy's type definitions.

**Phase:** Phase 1 (Component Port). Systematic replacement needed for every ported component.

---

### Pitfall 10: Legal Action Computation Location

**What goes wrong:** Alchemy computes `legalActions` on the frontend via `enumerateLegalActions(gameState, humanPlayer)` synchronously after every dispatch (gameStore.ts:172). The result is immediately available for UI rendering (highlighting valid attackers, valid targets, playable cards). Forge.rs computes legal actions on the WASM/engine side -- the frontend doesn't have an `enumerateLegalActions` function. The current Forge.rs store doesn't even have a `legalActions` array.

**Why it happens:** Alchemy's `CreatureSlots` builds an `actionIndex` (lines 145-213) from `legalActions` to determine which creatures can attack, block, or be targeted. Every card interaction highlight depends on this array. Without it, the ported UI has no way to show valid moves.

**Consequences:** No visual feedback for legal moves. Players can't tell which creatures can attack or block. Targeting shows no valid targets. The UI feels broken even though the engine works.

**Prevention:**
- Add a `getLegalActions()` export to the WASM bridge that returns the set of legal actions for the current player. This must be called after every state update.
- Store `legalActions` in Forge.rs's `gameStore` alongside `gameState`.
- The `actionIndex` pattern from Alchemy's `CreatureSlots` is excellent and should be ported directly -- it pre-indexes legal actions by type for O(1) lookup.
- Be aware that Forge.rs's `GameAction` shape differs from Alchemy's: `DeclareAttackers { data: { attacker_ids: ObjectId[] } }` (batch) vs Alchemy's `DECLARE_ATTACKER { permanentId: string }` (individual). The `actionIndex` will need structural changes.

**Detection:** Start a game. If no cards in hand glow as "playable" and no creatures highlight as valid attackers, legal actions aren't being surfaced.

**Confidence:** HIGH -- structural difference between sync frontend validation (Alchemy) and engine-side validation (Forge.rs).

**Phase:** Phase 1 (Adapter Integration). Legal actions must be available before any interactive UI works.

---

## Minor Pitfalls

---

### Pitfall 11: CSS Custom Property Naming Collision

**What goes wrong:** Both projects use CSS custom properties for card sizing. Alchemy uses `--card-font-scale`, `--board-card-width`, `--board-card-height`, `--_board-w`, `--_board-h`. Forge.rs uses `--card-h`. If both sets of properties exist during the port, components read the wrong values.

**Prevention:** Audit all CSS custom properties in both projects. Standardize on one naming convention. Prefer Alchemy's more descriptive names.

**Phase:** Phase 1 (Board Layout). Resolve before any visual rendering.

---

### Pitfall 12: Player ID Type Mismatch

**What goes wrong:** Alchemy uses string literal union: `PlayerId = 'player1' | 'player2'`. Forge.rs uses numeric: `PlayerId = number` (0 for player, 1 for opponent). Position registry keys in Alchemy use `player:${playerId}` (e.g., `player:player1`). Forge.rs would produce `player:0`. All position lookups for player targeting will silently fail.

**Prevention:** Normalize player IDs at the adapter layer. Either convert numeric IDs to string labels or update all template literals. The position registry pattern is used extensively in the animation system and must be consistent.

**Detection:** Try to target a player with a spell. If the targeting glow doesn't appear on the player's avatar/HUD, the player ID format is wrong in the position registry.

**Confidence:** HIGH -- directly from type definitions.

**Phase:** Phase 1 (Adapter Layer). One-time mapping.

---

### Pitfall 13: Animation Store Intermediate Display State

**What goes wrong:** Alchemy's animation store tracks `displayHealth`, `displayCreatureDamage`, `previousDisplayHealth`, and `previousDisplayCreatureDamage` for per-step animated health changes. These use Alchemy's `PlayerId` (`'player1' | 'player2'`) as keys and `permanentId` strings for creature damage. Forge.rs uses numeric player IDs and `ObjectId` (number) for permanents. The `applyStepHealthDeltas` and `applyStepCreatureDamage` functions hardcode Alchemy's player ID strings.

**Prevention:** The animation store's health/damage tracking functions need the same ID normalization as the rest of the system. If player IDs are mapped at the adapter layer (Pitfall 12), this resolves automatically. But if creature IDs change format (string to number), the damage tracking maps will silently miss updates.

**Phase:** Phase 2 (Animation System). Ensure ID consistency across all stores.

---

## Phase-Specific Warnings

| Phase Topic | Likely Pitfall | Mitigation |
|-------------|---------------|------------|
| Board Layout (Phase 1) | Fixed slots break with unlimited permanents (1) | Multi-row layout with separate rows per card type, aggressive token stacking |
| Board Layout (Phase 1) | Phase/combat state shape mismatch (7) | `useCombatState()` hook normalizing both models |
| Component Adapter (Phase 1) | Type shape mismatch -- flat vs deep (3) | View model mapping layer between `GameObject` and component props |
| Card Rendering (Phase 1) | Static vs async image loading (5) | `useCardImage` hook with loading skeleton, batch prefetch |
| Card Rendering (Phase 1) | Alchemy concepts don't map (9) | Systematic element -> mana color replacement, remove learning/energy UI |
| Adapter Integration (Phase 1) | Legal actions not available (10) | Add `getLegalActions()` to WASM bridge, store in gameStore |
| Adapter Integration (Phase 1) | Player ID type mismatch (12) | Normalize at adapter layer |
| Adapter Integration (Phase 1) | WASM Map serialization (8) | Existing `convertMapsToRecords`, consider typed getters |
| Animation System (Phase 2) | Sync vs async dispatch timing (2) | Serialize dispatch-animate flow, capture positions after async resolves |
| Animation System (Phase 2) | Event shape mismatch (6) | Event mapping layer before `groupEventsIntoSteps` |
| Animation System (Phase 2) | Intermediate display state ID formats (13) | Consistent ID normalization |
| MTG-Specific UI (Phase 3) | No stack/priority/instant UI (4) | New components, not ports. Stack display, priority controls, mana payment modal |
| Styling (All phases) | CSS custom property collisions (11) | Audit and standardize naming |

---

## Sources

- Alchemy `types.ts` (`/Users/matt/dev/alchemy/src/engine/types.ts`) -- Alchemy type system analysis
- Alchemy `CreatureSlots.tsx` -- Board layout with fixed slots, stacking logic
- Alchemy `gameStore.ts` -- Synchronous dispatch pattern
- Alchemy `animationStore.ts` -- Animation queue and event grouping
- Alchemy `BoardCard.tsx` -- Card rendering with static art
- Alchemy `CardFace.tsx` -- Card face rendering with element-based art paths
- Alchemy `GameBoard.tsx` -- Board layout composition
- Forge.rs `adapter/types.ts` -- Engine type system (WASM boundary)
- Forge.rs `wasm-adapter.ts` -- Async dispatch, Map conversion
- Forge.rs `stores/gameStore.ts` -- Async dispatch pattern
- Forge.rs `components/board/GameBoard.tsx` -- Existing multi-row battlefield layout
- Forge.rs `components/board/BattlefieldRow.tsx` -- Dynamic permanent row rendering
- Forge.rs `services/scryfall.ts` -- Async card image loading with rate limiting
- Forge.rs `hooks/useCardImage.ts` -- Async image hook with cache
