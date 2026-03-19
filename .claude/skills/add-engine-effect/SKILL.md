---
name: add-engine-effect
description: Use when adding a new effect, mechanic, or parser capability to the engine. Covers the full lifecycle from types → parser → resolver → targeting → frontend → AI → tests.
---

# Adding a New Effect to the Engine

This is the authoritative checklist for adding a new effect (mechanic, keyword action, etc.) to the phase.rs engine. Every step has a **file path** and a **what to do**. Missing any step causes silent failures — effects parse but don't resolve, resolve but don't target, target but don't animate, etc.

**Before you start:** Read the existing effect most similar to yours. Trace it through every file below. This is how you learn the patterns — not by guessing.

---

## Design Philosophy — Composability Over Completeness

**The single most important principle:** An effect should do ONE thing. Multi-step card abilities are composed from chains of single-purpose effects linked by `sub_ability`.

### Compose, don't monolith

When a card says "Search your library for a creature card, reveal it, put it into your hand, then shuffle your library," that's **four** effects composed:

1. `SearchLibrary { filter, count, reveal }` — find the card
2. `ChangeZone { destination: Hand }` — put it into hand (injected as sub_ability)
3. `Shuffle {}` — shuffle library (parsed from ", then shuffle")

Each is a reusable building block. `SearchLibrary` works for tutors, `ChangeZone` works for bounce/exile/reanimate, `Shuffle` works independently. The composition unlocks hundreds of cards, not just one.

**Anti-pattern:** Creating `Effect::SearchAndPutInHandAndShuffle` — handles exactly one card template, can't be recomposed.

### Building blocks unlock future cards

Before implementing a specific card's text, ask: "What is the **general pattern** here?" Recent examples from this codebase:

- `contains_possessive()` / `starts_with_possessive()` — match "your"/"their"/"its owner's" variants in Oracle text. One helper, used by `SearchLibrary`, `Shuffle`, `ChangeZone`, and future effects.
- `contains_object_pronoun()` — match "it"/"them"/"that card"/"those cards". Unlocks ~2000+ card patterns.
- `match_phrase_variants()` — shared logic that eliminated duplication across all phrase-matching helpers.

When you build a helper like this, you're not adding complexity — you're **reducing** the cost of every future effect.

### Sub-ability chains: how multi-step effects work

`parse_effect_chain()` splits Oracle text on sentence boundaries and links each parsed effect as a `sub_ability` on the previous one. At runtime, `resolve_ability_chain()` walks this chain, executing each step.

**Target propagation:** Parent targets flow to sub-abilities automatically. "Exile target creature. Its controller gains life equal to its power" → the sub-ability for `GainLife` receives the creature target from the parent `ChangeZone`, without duplicating target data.

**Continuation pattern:** For interactive effects (reveal hand → choose card → exile it), use `pending_continuation`. The resolver sets `WaitingFor` and stashes the remaining sub-ability chain. When the player responds, `engine.rs` picks up the continuation.

### When to extend an effect vs. compose a new one

- **Extend:** The new behavior is a variation of an existing effect (e.g., adding `DamageAmount::TargetToughness` alongside `DamageAmount::TargetPower`).
- **Compose:** The new behavior is a sequence of existing effects (e.g., "exile then return" = `ChangeZone` to exile + `ChangeZone` to battlefield).
- **New effect:** The behavior is genuinely novel and can't be expressed by existing variants (e.g., `Explore`, `Proliferate`).

---

## The Checklist

### Phase 1 — Type Definition

**Goal:** The effect exists as a typed variant in the data model.

- [ ] **`crates/engine/src/types/ability.rs` — `Effect` enum**
  Add the new variant with typed fields. Follow these rules:
  - Use enum fields for semantically distinct data (e.g., `DamageAmount::Fixed(i32)` vs `DamageAmount::TargetPower`). **Never use boolean flags** as substitutes for proper enum variants.
  - Mark optional/new fields `#[serde(default)]` so existing `card-data.json` stays deserializable.
  - If the effect targets something, include a `target: TargetFilter` field.
  - If the effect has a duration, include a `duration: Option<Duration>` field.

- [ ] **`crates/engine/src/types/ability.rs` — `effect_variant_name()`**
  Add a match arm returning the variant's string name (e.g., `"RevealHand"`). Used in `GameEvent::EffectResolved` and logging.

- [ ] **`crates/engine/src/types/ability.rs` — `EffectKind` enum + `From<&Effect>`**
  Add an `EffectKind` variant and the corresponding `From<&Effect>` match arm. `EffectKind` is used for effect categorization and coverage tracking.

### Phase 2 — Effect Handler (Resolver)

**Goal:** The engine can execute the effect at runtime.

- [ ] **`crates/engine/src/game/effects/<name>.rs` — Create resolver module**
  Write a `pub fn resolve(state, ability, events) -> Result<(), EffectError>` function.
  - Extract typed fields from `ability.effect` via pattern match.
  - Find targets from `ability.targets` (resolved `TargetRef` values).
  - Mutate `state`, push `GameEvent`s to `events`.
  - **Never access card data or parse text** in resolvers — only process the typed `ResolvedAbility`.

- [ ] **`crates/engine/src/game/effects/mod.rs` — `resolve_effect()` match arm**
  Wire the new variant: `Effect::YourEffect { .. } => your_module::resolve(state, ability, events)`.
  Also add `mod your_module;` at the top.

### Phase 3 — Targeting Integration

**Goal:** The engine correctly requests and validates targets for this effect.

Only needed if the effect has a `target: TargetFilter` field that isn't `None`/`SelfRef`/`Controller`.

- [ ] **`crates/engine/src/game/triggers.rs` — `extract_target_filter_from_effect()`**
  Add the variant to the match arm that extracts `&TargetFilter` from effects. **If you skip this, triggered abilities with this effect will go on the stack without requesting targets — they'll silently fail at resolution.**

- [ ] **`crates/engine/src/game/targeting.rs` — verify `find_legal_targets()`**
  Check that the `TargetFilter` values your effect uses are actually resolvable. Common gaps:
  - `TargetFilter::Typed { controller: Opponent, card_type: None }` → wants to target an opponent as a **player**, but typed targeting only searches battlefield objects. You may need to add player resolution for typed filters.
  - Custom `FilterProp` values → ensure they're handled in the filter matching logic.
  - **Zone-aware targeting:** If the effect targets cards in non-battlefield zones (graveyard, exile, hand, library), the parser should produce `FilterProp::InZone { zone }` in the target filter properties. When `InZone` is present, `find_legal_targets` searches ONLY that zone exclusively. Per MTG rule 702.16a, hexproof/shroud only apply on the battlefield — non-battlefield targeting checks only protection.

### Phase 4 — Parser

**Goal:** Oracle text for this effect produces the correct typed `Effect` variant.

- [ ] **`crates/engine/src/parser/oracle_effect/` — the right `parse_*_ast()` helper**
  Most new effect parser work now belongs in a typed imperative-family helper, not directly in a flat verb list. Register the pattern in the smallest matching parser:
  - `parse_numeric_imperative_ast()`
  - `parse_zone_counter_ast()`
  - `parse_cost_resource_ast()`
  - `parse_targeted_action_ast()`
  - `parse_search_and_creation_ast()`
  - `parse_hand_reveal_ast()`
  - `parse_choose_ast()`
  - `parse_put_ast()`
  - `parse_shuffle_ast()`
  - `parse_utility_imperative_ast()`

- [ ] **`crates/engine/src/parser/oracle_effect/` — subject-preserving helpers (if needed)**
  If the subject of the sentence carries game-relevant information (who does it, who it targets), preserve it before fallback subject stripping. The current pattern is `try_parse_targeted_controller_gain_life()` in `lower_imperative_clause()`. See `docs/parser-instructions.md` § "Subject Stripping" for the full rationale.

- [ ] **`crates/engine/src/parser/oracle_effect/` — `parse_effect_chain()` integration**
  If the effect commonly appears as part of a multi-sentence ability (e.g., "Look at target opponent's hand. You may choose a nonland card from it. Exile that card."), ensure the sentence splitting and `sub_ability` chaining produces the right structure. The engine composes effects via `sub_ability` chains — don't create mega-effects that do multiple things.

  **Absorption pattern:** Some sentences modify a preceding effect rather than creating a new sub_ability. Examples: RevealHand absorbs card filters, SearchLibrary absorbs destinations, `Effect::Mana` absorbs "Spend this mana only to cast..." clauses as `ManaSpendRestriction`, and `Effect::Counter` can absorb follow-up `source_static`. If your new effect has modifier sentences, add absorption logic in `parse_effect_chain()` — don't emit them as separate sub_abilities.

- [ ] **`crates/engine/src/parser/oracle_effect/` — parser tests**
  Every new pattern **must** have a `#[test]` in the inline test module:
  ```rust
  #[test]
  fn effect_your_new_pattern() {
      let e = parse_effect("Oracle text here");
      assert!(matches!(e, Effect::YourEffect { field: expected, .. }));
  }
  ```

#### Parser Helpers Reference

These helpers live in `oracle_effect/` and `oracle_util.rs`. Know them — they eliminate entire classes of edge cases.

**Possessive helpers** — detect who "owns" something without requiring explicit targeting:

| Helper | Matches | Example Oracle text |
|--------|---------|---------------------|
| `contains_possessive(s)` | "your", "their", "its owner's", "his or her" | "Look at your library" |
| `starts_with_possessive(s)` | same, anchored at start | "Your hand is revealed" |
| `contains_object_pronoun(s)` | "it", "them", "that card", "those cards", "the card" | "Exile it" / "Return them" |

**Targeting helper** — use when Oracle text says "target …":

```
parse_target(text) -> Option<(TargetFilter, &str)>
```

Returns the matched `TargetFilter` **and the remaining text** after consuming the "target …" phrase. The leftover is discarded or fed to further parsing. Example:

- Input: `"target opponent's hand"`
- Consumes: `"target opponent"` (matches `TargetFilter::Typed { controller: Opponent, .. }`)
- Leftover: `"'s hand"` — discard

**The possessive vs. targeting fork** — the single most important parser decision:

```
"Look at your/their hand"         → contains_possessive matches
                                  → target = Controller or Any (no WaitingFor needed)

"Look at target opponent's hand"  → parse_target matches "target opponent"
                                  → target = TargetFilter::Typed { controller: Opponent }
                                  → requires targeting phase (triggers.rs + stack.rs)
```

Getting this wrong produces silent failures: possessive forms that fall through to `parse_target` will silently produce no target (or `Unimplemented`); targeting forms that match `contains_possessive` will skip the targeting phase entirely.

**`match_phrase_variants()`** — shared backbone for all phrase helpers. If you need a new phrase helper (e.g., `contains_sacrifice_clause`), implement it via `match_phrase_variants` rather than duplicating the normalization logic.

### Phase 5 — Interactive Effects (if applicable)

**Goal:** Effects that require player choices (scry, dig, reveal+choose, search) work end-to-end.

Only needed if the effect pauses for player input.

- [ ] **`crates/engine/src/types/game_state.rs` — `WaitingFor` enum**
  Add a variant (e.g., `RevealChoice { player, cards, filter }`) that carries enough data for the frontend to render the choice UI.

- [ ] **`crates/engine/src/game/engine.rs` — `apply()` match arm**
  Handle the `(WaitingFor::YourChoice, GameAction::YourResponse)` pair. This is where the player's choice feeds back into the engine.

- [ ] **`crates/engine/src/types/game_state.rs` — `GameAction` enum**
  Add a variant for the player's response if no existing action fits.

- [ ] **`client/src/adapter/types.ts` — `WaitingFor` type**
  Add the TypeScript discriminated union variant. Note: `tsify` auto-generates types from Rust, but the `types.ts` file may have manual overrides — check both.

- [ ] **`client/src/components/` — UI for the choice**
  Build (or extend) a component that renders when `waitingFor.type === "YourChoice"`. Current patterns:
  - `client/src/components/modal/CardChoiceModal.tsx` for `ScryChoice`, `DigChoice`, `SurveilChoice`, `RevealChoice`, `SearchChoice`, `DiscardToHandSize`
  - `client/src/components/modal/NamedChoiceModal.tsx` for `NamedChoice`
  - `client/src/components/modal/ModeChoiceModal.tsx` for `ModeChoice` and `AbilityModeChoice`

### Phase 5b — Multiplayer State Filtering (if applicable)

**Goal:** Hidden information is correctly filtered in multiplayer games.

Only needed if the effect reveals or hides information (hands, face-down cards, library contents).

- [ ] **`crates/server-core/src/filter.rs` — `filter_state_for_player()`**
  Ensure the filter respects the new revealed/hidden state. For example, `RevealHand` adds card IDs to `state.revealed_cards`, and `filter_state_for_player` checks this set before hiding opponent hand cards. If your effect exposes hidden information, it must be visible through the filter.

- [ ] **`crates/server-core/src/session.rs`**
  If the new `WaitingFor` variant should be sent to specific players (not broadcast), handle the routing here.

### Phase 6 — Frontend Events & Animations (if applicable)

**Goal:** The frontend reacts visually to the effect.

- [ ] **`crates/engine/src/types/events.rs` — `GameEvent` enum**
  Add an event variant if the effect needs to be visible in the game log or trigger animations.

- [ ] **`client/src/adapter/types.ts` — `GameEvent` type**
  Add the TypeScript variant.

- [ ] **`client/src/components/log/` — Game log rendering**
  Handle the new event in the log component so players see what happened.

### Phase 7 — AI (if applicable)

**Goal:** The AI can make reasonable decisions when this effect is in play.

- [ ] **`crates/phase-ai/src/eval.rs` — effect evaluation**
  If the effect materially changes board evaluation (e.g., exile vs. destroy), ensure the AI's state evaluator accounts for it.

- [ ] **`crates/engine/src/ai_support/candidates.rs` — action generation**
  If the effect introduces a new `WaitingFor` / `GameAction` pair, the AI needs to generate legal responses. Legal action generation lives in `engine::ai_support::legal_actions()` → `candidate_actions()`.

### Phase 8 — Verification

- [ ] **`cargo test -p engine`** — All existing tests pass.
- [ ] **`cargo clippy --all-targets -- -D warnings`** — No warnings.
- [ ] **`cargo fmt --all`** — Formatted.
- [ ] **Snapshot test** — If the effect changes a card's parsed abilities, update or add an `insta` snapshot in `crates/engine/tests/oracle_parser.rs`.
- [ ] **`cargo coverage`** — Run the coverage report to verify the new effect reduces `Unimplemented` count for the target cards.

---

## Common Mistakes

| Mistake | Consequence | Fix |
|---------|-------------|-----|
| Missing `extract_target_filter_from_effect` arm | Triggered abilities skip targeting — resolve with no targets, hit `MissingParam` error | Add the variant to the match in `triggers.rs:409` |
| Missing `extract_target_filter_from_effect` arm in `stack.rs` | Spells don't fizzle when targets become illegal | Ensure the effect's `TargetFilter` is returned by `extract_target_filter_from_effect` |
| Creating a mega-effect instead of composing | One-off solution that handles one card pattern | Decompose into building blocks with `sub_ability` chains |
| Boolean flags on `Effect` variants | Undefined combinations, unclear intent | Use enum variants (see `DamageAmount`, `LifeAmount`) |
| Added parser logic in the wrong AST helper | Pattern parses inconsistently or bypasses continuation absorption | Register it in the smallest matching `parse_*_ast()` helper |
| Imperative fallback only, no subject preservation | "Its controller gains life equal to its power" loses context | Preserve the subject before fallback subject stripping |
| Using `parse_target` for possessive forms ("your hand") | Silently produces no target or `Unimplemented` | Use `contains_possessive` → `target: Controller` instead |
| Using `contains_possessive` for targeting forms ("target opponent's hand") | Skips targeting phase, effect fires with no target | Use `parse_target` → `TargetFilter::Typed { controller: Opponent }` |
| Hardcoding `amount: 1` as fallback | Silently wrong behavior, not visible in coverage | Return `Unimplemented` so the gap shows up |
| Adding `TargetFilter::Typed` without player resolution | "Target opponent" as a player never resolves | Check `find_legal_targets` handles your filter |
| Forgetting `#[serde(default)]` on new fields | Old `card-data.json` files fail to deserialize | Always default new optional fields |
| Effect reveals info but no `filter.rs` update | Opponent sees hidden cards in multiplayer, or revealed cards stay hidden | Update `filter_state_for_player` in `server-core/src/filter.rs` |
| Building a multi-step mega-effect | One-off variant that only works for one card template | Decompose into `sub_ability` chain of single-purpose effects |
| Missing AI legal action generation | AI hangs on `WaitingFor` with no valid response | Add the `WaitingFor` variant to `engine/src/ai_support/candidates.rs` |

---

## Architecture Refresher

```
Oracle text (MTGJSON)
  │
  ▼
Parser (oracle_effect/) ──► Effect variant (types/ability.rs)
  │                              │
  ▼                              ▼
AbilityDefinition ──────► card-data.json (exported)
                              │
                              ▼
                         GameState loads card DB
                              │
                              ▼
                    Card is cast/triggered → goes on stack
                              │
                              ▼
                    Stack resolves → extract_target_filter (triggers.rs / stack.rs)
                              │
                              ▼
                    resolve_effect (effects/mod.rs) → your handler
                              │
                              ▼
                    GameEvent emitted → frontend renders
                              │
                              ▼
                    WaitingFor (if interactive) → GameAction response → engine.rs apply()
```

---

## Reference: Existing Effects Worth Studying

| Pattern | Example Effect | Why it's useful |
|---------|---------------|-----------------|
| Simple targeted | `Destroy { target }` | Minimal example of target → resolve |
| All-permanents | `DestroyAll { filter }` | No targeting, filter-based |
| Interactive choice | `Scry { count }` | WaitingFor + GameAction round-trip |
| Multi-step chain | `SearchLibrary` → `ChangeZone` → `Shuffle` | Sub-ability composition |
| Player targeting | `RevealHand { target }` | Targets a player, not an object |
| Computed amount | `DealDamage { amount: DamageAmount::TargetPower }` | Dynamic value resolution |
| Self-referencing | `AddCounter { target: SelfRef }` | `~` normalization in parser |
