# Minor UI Fixes Session — 2026-03-08

## Bug 1: Player hand not rendering

**Symptom**: Mulligan dialog showed "Mulligan (0 cards)" with no card images. Player hand area was empty (0 children in DOM). Opponent hand rendered fine.

**Root cause**: `serde_wasm_bindgen` serializes Rust `HashMap<ObjectId, GameObject>` as a JS `Map` (not a plain object) because `ObjectId` is a newtype wrapping `u64`. The frontend uses bracket access (`objects[id]`) which doesn't work on `Map` — requires `.get(id)`.

**Diagnosis path**: React fiber inspection revealed `PlayerHand` component had `hand: [3, 34, 10, 49, 22, 13, 28]` (7 cards) but `objects` was `object(0 keys: )`. Further inspection confirmed `objects` was `instanceof Map` with 120 entries — the data existed but was inaccessible via bracket notation.

**Files changed**:
- `crates/engine/src/types/identifiers.rs` — Added `#[serde(transparent)]` to `CardId` and `ObjectId`
- `crates/engine/src/types/player.rs` — Added `#[serde(transparent)]` to `PlayerId`
- `client/src/adapter/wasm-adapter.ts` — Added `convertMapsToRecords()` recursive utility in `fetchState()` to convert JS `Map` instances to plain `Record<string, V>` objects

**Note**: The `#[serde(transparent)]` helps for JSON serialization but doesn't change `serde_wasm_bindgen`'s behavior for numeric HashMap keys — the adapter-layer conversion is the actual fix. It also covers `CombatState`'s `HashMap<ObjectId, ...>` fields.

## Bug 2: Can't cast spells — "Card has no abilities"

**Symptom**: Clicking any card in hand produced `Engine error: Invalid action: Card has no abilities`.

**Root cause**: `casting.rs:52` rejected cards with an empty `abilities` vec. Vanilla creatures like Suntail Hawk (whose only text is the keyword "Flying") have no spell-ability text, so `obj.abilities` is empty.

**File changed**: `crates/engine/src/game/casting.rs` — When `obj.abilities` is empty, creates a default `AbilityDefinition { api_type: "PermanentNoncreature", kind: AbilityKind::Spell, params: {} }` so vanilla permanents are castable.

## Bug 3: Can't cast spells — "Cannot pay mana cost"

**Symptom**: Even with untapped lands on the battlefield, casting any spell fails with "Cannot pay mana cost".

**Root cause**: `pay_and_push()` checked `can_pay(&player_data.mana_pool, cost)` but the mana pool was always empty — there was no auto-tap logic. The engine expected the player to manually tap lands first via `TapLandForMana` actions, but the frontend had no UI for that (clicking a land only selected it visually).

**File changed**: `crates/engine/src/game/casting.rs` — Added `auto_tap_lands()` function called before `can_pay` check. Strategy: tap lands producing colors matching colored shard requirements first, then any remaining untapped lands for generic costs. Uses existing `mana_payment::land_subtype_to_mana_type()` and `mana_payment::produce_mana()` APIs.

## Verified Working After Fixes

- Menu → deck selection → difficulty → game start
- Mulligan flow with card images from Scryfall
- Playing lands from hand to battlefield
- Phase/turn progression (Untap → Draw → M1 → Combat → M2 → End → Cleanup)
- AI taking actions (mulliganing, playing lands)
- Life totals, game log, phase tracker, stack display

## Still Needs Testing (post-WASM rebuild)

- Casting creatures and spells with auto-tap
- Combat (declare attackers/blockers)
- Spell resolution (stack resolution putting permanents onto battlefield)

## Backlog

### Card container height mismatch in mulligan modal
**Symptom**: Cards in the mulligan modal (and possibly other card lists) have a parent container that is slightly taller than the card itself — there's a visible gap below the card image.
**Expected**: Container should match the card's dimensions exactly (no extra vertical space).
**Likely locations**: `MulliganModal.tsx`, `CardImage.tsx`, or the card wrapper div styling.
