---
phase: quick-02
plan: 01
type: execute
wave: 1
depends_on: []
files_modified:
  - client/src/components/zone/LibraryPile.tsx
  - client/src/adapter/types.ts
  - crates/engine/src/game/static_abilities.rs
  - crates/engine-wasm/src/lib.rs
  - crates/engine/src/types/player.rs
autonomous: true
requirements: [QUICK-02]

must_haves:
  truths:
    - "LibraryPile displays correct count from library.length (not broken library_size)"
    - "LibraryPile shows top card face-up when player has MayLookAtTopOfLibrary static effect active"
    - "LibraryPile shows card-back when no peek effect is active (default behavior)"
  artifacts:
    - path: "client/src/components/zone/LibraryPile.tsx"
      provides: "Library pile with conditional peek support"
    - path: "crates/engine/src/game/static_abilities.rs"
      provides: "MayLookAtTopOfLibrary static ability handler"
    - path: "crates/engine-wasm/src/lib.rs"
      provides: "can_look_at_top_of_library derived field computation"
  key_links:
    - from: "crates/engine-wasm/src/lib.rs"
      to: "crates/engine/src/game/static_abilities.rs"
      via: "check_static_ability call in get_game_state"
      pattern: 'check_static_ability.*MayLookAtTopOfLibrary'
    - from: "client/src/components/zone/LibraryPile.tsx"
      to: "client/src/adapter/types.ts"
      via: "Player.can_look_at_top_of_library field"
      pattern: "can_look_at_top_of_library"
---

<objective>
Fix LibraryPile bug and add library peek support for "You may look at the top card of your library" effects.

Purpose: The LibraryPile currently references a nonexistent `library_size` field (silently evaluates to 0 via `?? 0` fallback). The library also needs to support conditionally revealing the top card when a static ability like Future Sight or Courser of Kruphix is active.

Output: Working LibraryPile with correct count + conditional top-card peek, engine-side MayLookAtTopOfLibrary static ability check, WASM-bridge derived field.
</objective>

<execution_context>
@/Users/matt/.claude/get-shit-done/workflows/execute-plan.md
@/Users/matt/.claude/get-shit-done/templates/summary.md
</execution_context>

<context>
@client/src/components/zone/LibraryPile.tsx
@client/src/components/zone/GraveyardPile.tsx
@client/src/adapter/types.ts
@crates/engine/src/types/player.rs
@crates/engine/src/game/static_abilities.rs
@crates/engine-wasm/src/lib.rs
@client/src/pages/GamePage.tsx
</context>

<interfaces>
<!-- Key types and contracts the executor needs. -->

From client/src/adapter/types.ts:
```typescript
export interface Player {
  id: PlayerId;
  life: number;
  mana_pool: ManaPool;
  library: ObjectId[];
  hand: ObjectId[];
  graveyard: ObjectId[];
  has_drawn_this_turn: boolean;
  lands_played_this_turn: number;
}
```

From crates/engine/src/types/player.rs:
```rust
pub struct Player {
    pub id: PlayerId,
    pub life: i32,
    pub mana_pool: ManaPool,
    pub library: Vec<ObjectId>,
    pub hand: Vec<ObjectId>,
    pub graveyard: Vec<ObjectId>,
    pub has_drawn_this_turn: bool,
    pub lands_played_this_turn: u8,
    pub poison_counters: u32,
}
```

From crates/engine/src/game/static_abilities.rs:
```rust
pub fn check_static_ability(state: &GameState, mode: &str, context: &StaticCheckContext) -> bool;
// Registry uses handle_rule_mod for simple mode strings
```

From crates/engine-wasm/src/lib.rs (get_game_state pattern):
```rust
// Derived fields computed before serialization:
obj.has_unimplemented_mechanics = engine::game::coverage::has_unimplemented_mechanics(obj);
obj.has_summoning_sickness = engine::game::combat::has_summoning_sickness(obj, turn);
```

From client/src/components/zone/GraveyardPile.tsx (reference for TopCardArt pattern):
```typescript
function TopCardArt({ cardName }: { cardName: string }) {
  const { src } = useCardImage(cardName, { size: "art_crop" });
  // renders img or gray placeholder
}
```
</interfaces>

<tasks>

<task type="auto">
  <name>Task 1: Add MayLookAtTopOfLibrary engine support and WASM derived field</name>
  <files>crates/engine/src/game/static_abilities.rs, crates/engine/src/types/player.rs, crates/engine-wasm/src/lib.rs, client/src/adapter/types.ts</files>
  <action>
1. In `crates/engine/src/game/static_abilities.rs`, register "MayLookAtTopOfLibrary" in `build_static_registry()` using `handle_rule_mod` (same pattern as CantGainLife, CantDraw, etc.).

2. In `crates/engine/src/types/player.rs`, add a derived field to `Player`:
   ```rust
   #[serde(skip_deserializing, default)]
   pub can_look_at_top_of_library: bool,
   ```
   This follows the same `skip_deserializing` pattern used for `has_unimplemented_mechanics` on GameObject. Update the `Default` impl to include `can_look_at_top_of_library: false`.

3. In `crates/engine-wasm/src/lib.rs`, in the `get_game_state()` function, after the existing object loop, add a player loop that checks the static ability for each player:
   ```rust
   use engine::game::static_abilities::{check_static_ability, StaticCheckContext};

   for player in state.players.iter_mut() {
       let ctx = StaticCheckContext {
           player_id: Some(player.id),
           ..Default::default()
       };
       player.can_look_at_top_of_library =
           check_static_ability(state, "MayLookAtTopOfLibrary", &ctx);
   }
   ```
   Note: `state.players` is a Vec, so iterate mutably. The borrow issue with `check_static_ability` needing `&state` while iterating `&mut state.players` means you should compute the values first into a vec of bools, then assign them. Compute: `let peek_flags: Vec<bool> = state.players.iter().map(|p| { let ctx = ...; check_static_ability(state, "MayLookAtTopOfLibrary", &ctx) }).collect();` then `for (i, flag) in peek_flags.into_iter().enumerate() { state.players[i].can_look_at_top_of_library = flag; }`.

4. In `client/src/adapter/types.ts`, add `can_look_at_top_of_library?: boolean;` to the `Player` interface (optional since older states won't have it).
  </action>
  <verify>
    <automated>cd /Users/matt/dev/forge.rs && cargo test -p engine -- static && cargo test -p engine-wasm 2>&1 | tail -5</automated>
  </verify>
  <done>MayLookAtTopOfLibrary registered in static registry, Player has can_look_at_top_of_library derived field, WASM bridge computes it per player, TS type updated</done>
</task>

<task type="auto">
  <name>Task 2: Fix LibraryPile count bug and add conditional peek rendering</name>
  <files>client/src/components/zone/LibraryPile.tsx</files>
  <action>
1. Fix the count selector bug: change `s.gameState?.players[playerId]?.library_size ?? 0` to `s.gameState?.players[playerId]?.library?.length ?? 0`.

2. Add a `canPeek` selector that reads `can_look_at_top_of_library` from the player state (only for the human player, playerId 0): `const canPeek = useGameStore((s) => playerId === 0 && (s.gameState?.players[playerId]?.can_look_at_top_of_library ?? false));`

3. Add a `topCardName` selector (matching GraveyardPile's pattern) that resolves the name of the top card of the library when peek is active: `const topCardName = useGameStore((s) => { if (!canPeek) return null; const lib = s.gameState?.players[playerId]?.library; if (!lib || lib.length === 0) return null; return s.gameState?.objects[lib[0]]?.name ?? null; });`. Note: library top is index 0 (convention: library[0] = top, per zones.rs:91). This differs from graveyard where top = last element.

4. Reuse the `TopCardArt` pattern from GraveyardPile — create a local `TopCardArt` component (or extract and share from GraveyardPile — but to keep it simple and avoid touching GraveyardPile, duplicate the small component locally).

5. When `canPeek` is true and `topCardName` is available, render the top card art inside the library pile instead of the card-back pattern. Add a small eye icon badge or a subtle cyan border to indicate peek mode. When `canPeek` is false, keep the existing card-back rendering unchanged.

6. The peek rendering should look like: replace the card-back `div` (the gradient `from-indigo-950 to-gray-900` with the amber border inner div) with `<TopCardArt cardName={topCardName} />` when peeking. Add `border-cyan-600` instead of `border-gray-600` on the top card container when peeking, to visually indicate the peek state.
  </action>
  <verify>
    <automated>cd /Users/matt/dev/forge.rs/client && pnpm run type-check 2>&1 | tail -5</automated>
  </verify>
  <done>LibraryPile shows correct count from library.length, displays top card art when can_look_at_top_of_library is true with cyan border indicator, shows card-back pattern otherwise</done>
</task>

</tasks>

<verification>
- `cargo test -p engine` passes (static ability registry)
- `cd client && pnpm run type-check` passes (TS types aligned)
- LibraryPile uses `library.length` (not `library_size`)
- LibraryPile conditionally shows top card when `can_look_at_top_of_library` is true
</verification>

<success_criteria>
- Library pile displays correct card count using library.length
- MayLookAtTopOfLibrary static ability registered in engine
- WASM bridge computes can_look_at_top_of_library per player
- LibraryPile shows top card face-up with cyan border when peek is active
- LibraryPile shows card-back (unchanged behavior) when no peek effect
- All Rust tests pass, TypeScript type-check passes
</success_criteria>

<output>
After completion, create `.planning/quick/2-add-graveyard-and-library-zone-stacks-wi/2-SUMMARY.md`
</output>
