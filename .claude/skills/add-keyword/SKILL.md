---
name: add-keyword
description: Use when adding or modifying keyword abilities — evergreen keywords (flying, trample), parameterized keywords (ward {2}, kicker {R}), protection variants, or any keyword that needs runtime behavior wired into combat, targeting, damage, or triggers.
---

# Adding a Keyword Ability

Keywords are strongly-typed abilities stored on GameObjects and evaluated across combat, targeting, damage, state-based actions, and triggers. They flow through the layer system (granted/removed via `ContinuousModification`) and are parsed from both MTGJSON keyword arrays and Oracle text.

**Before you start:** Trace how `Ward` works end-to-end — it's a parameterized keyword with targeting interaction: `Keyword::Ward(ManaCost)` in `keywords.rs` → `FromStr` parsing → `targeting.rs` check → layer grant via `AddKeyword`.

---

## MTG Rules Reference

| Rule | What it governs | Engine implication |
|------|----------------|-------------------|
| **702.x** | Individual keyword definitions | Each keyword has specific rules for where it matters |
| **613.1f** | Keywords granted/removed in Layer 6 | `ContinuousModification::AddKeyword` / `RemoveKeyword` |
| **702.2** | Evergreen keywords (flying, haste, etc.) | Unit variants on `Keyword` enum |
| **702.16** | Protection | `Keyword::Protection(ProtectionTarget)` with multi-check in `protection_prevents_from()` |
| **702.21** | Ward | `Keyword::Ward(ManaCost)` checked in `targeting.rs` |

---

## Key Types

### `Keyword` Enum — `crates/engine/src/types/keywords.rs`

Three categories of variants:

| Category | Examples | Data |
|----------|---------|------|
| **Unit (simple)** | `Flying`, `Haste`, `Trample`, `Deathtouch`, `Lifelink`, `Vigilance`, `Reach`, `Defender`, `Menace`, `Indestructible`, `Hexproof`, `Shroud`, `Flash` | None |
| **Parameterized (u32)** | `Dredge(u32)`, `Modular(u32)`, `Renown(u32)`, `Fabricate(u32)`, `Annihilator(u32)`, `Crew(u32)` | Numeric value |
| **Parameterized (ManaCost)** | `Ward(ManaCost)`, `Kicker(ManaCost)`, `Cycling(ManaCost)`, `Flashback(ManaCost)`, `Equip(ManaCost)`, `Morph(ManaCost)` | Mana cost |
| **Special** | `Protection(ProtectionTarget)`, `Enchant(TargetFilter)`, `Landwalk(String)`, `EtbCounter { counter_type, count }` | Typed data |
| **Fallback** | `Unknown(String)` | Unparsed name — never matches at runtime, tracked in coverage |

### Storage on `GameObject` — `crates/engine/src/game/game_object.rs`

```rust
pub keywords: Vec<Keyword>,       // Current (modified by layers)
pub base_keywords: Vec<Keyword>,   // Original (reset each layer evaluation)
```

### Runtime Checking — `crates/engine/src/game/keywords.rs`

```rust
pub fn has_keyword(obj: &GameObject, keyword: &Keyword) -> bool {
    obj.keywords.iter()
        .any(|k| std::mem::discriminant(k) == std::mem::discriminant(keyword))
}
```

Uses **discriminant matching** — `Ward({W})` matches `Ward({2}{U})`. This is intentional: "does this creature have ward?" doesn't care about the cost. To check the specific parameter, pattern match directly.

Convenience functions: `has_flying()`, `has_haste()`, `has_flash()`, `has_hexproof()`, `has_shroud()`, `has_indestructible()`.

---

## Checklist — Adding a New Keyword

### Phase 1 — Type Definition

- [ ] **`crates/engine/src/types/keywords.rs` — `Keyword` enum**
  Add the variant. Choose the right category:
  - Unit: `YourKeyword,` (no data)
  - Parameterized (numeric): `YourKeyword(u32),`
  - Parameterized (cost): `YourKeyword(ManaCost),`
  - Special: struct-like variant with named fields

- [ ] **`crates/engine/src/types/keywords.rs` — `FromStr` impl**
  Add parsing in the `from_str()` match. Three locations depending on type:
  - **Unit keywords**: Add to the lowercase match block (~line 310-380). Pattern: `"yourkeyword" => Ok(Keyword::YourKeyword),`
  - **Parameterized (u32)**: Add to the split-on-colon block. Pattern: `"yourkeyword" => Ok(Keyword::YourKeyword(value.parse()?))`
  - **Parameterized (ManaCost)**: Add to the `parse_keyword_mana_cost()` delegation block. Pattern: `"yourkeyword" => parse_keyword_mana_cost(value).map(Keyword::YourKeyword)`

- [ ] **`crates/engine/src/types/keywords.rs` — `keyword_from_tagged()`**
  Add a match arm for JSON deserialization (externally-tagged format). This handles the `{"YourKeyword": data}` format from card-data.json.

- [ ] **`crates/engine/src/types/keywords.rs` — `Display` impl** (if exists)
  Add formatting for debug/log output.

### Phase 2 — Runtime Behavior

Where you add logic depends on what the keyword *does*. Most keywords only need one or two of these.

- [ ] **`crates/engine/src/game/combat.rs`** — If affects attack/block legality
  Keywords checked here: Flying, Reach, Defender, Menace, Fear, Intimidate, Skulk, Shadow, Horsemanship, Haste, Vigilance.
  - Evasion (can't be blocked by X): Add to blocker legality checks
  - Attack restriction (can't attack / must attack): Add to attacker validation
  - Vigilance-like (doesn't tap): Add to attack resolution

- [ ] **`crates/engine/src/game/combat_damage.rs`** — If affects damage calculation
  Keywords checked here: Deathtouch, Trample, Lifelink, Wither, Infect, First Strike, Double Strike.
  - Damage modification: Add to `assign_combat_damage()` or `apply_combat_damage()`
  - Damage timing: First Strike / Double Strike use two damage steps

- [ ] **`crates/engine/src/game/targeting.rs`** — If affects targeting legality
  Keywords checked here: Hexproof, Shroud, Ward, Protection.
  - Untargetable: Add to `is_legal_target()` checks
  - Conditional protection: Add to `protection_prevents_from()` in `keywords.rs`

- [ ] **`crates/engine/src/game/sba.rs`** — If affects state-based actions
  Keywords checked here: Indestructible (prevents destruction).
  - Add check in `check_state_based_actions()`

- [ ] **`crates/engine/src/game/casting.rs`** — If affects casting/costs
  Keywords checked here: Convoke, Delve, Improvise, Flash, Flashback, alternative costs.
  - Cost modification: Add to cost calculation
  - Timing: Add to `can_cast_at_instant_speed()` or similar

- [ ] **`crates/engine/src/game/triggers.rs`** — If keyword generates triggers
  Keywords that synthesize triggers: Prowess, Undying, Persist, Exalted, Extort.
  Pattern: In `process_triggers()`, check `obj.has_keyword(&Keyword::YourKeyword)` on the relevant `GameEvent`, then build a synthetic `ResolvedAbility`.

- [ ] **`crates/engine/src/game/keywords.rs`** — Optional convenience function
  If the keyword is checked in multiple files, add `pub fn has_your_keyword(obj: &GameObject) -> bool`.

### Phase 3 — Layer Integration

Keywords are granted/removed via `ContinuousModification::AddKeyword` / `RemoveKeyword` in Layer 6. This is already handled generically — no changes needed unless your keyword has special layer interactions.

**Special cases:**
- If the keyword is a CDA (like Changeling → all creature types), see the post-fixup block in `layers.rs` (~line 69-88) for the pattern.
- If the keyword modifies other layers (e.g., Devoid → colorless in Layer 5), you need additional `ContinuousModification` variants.

### Phase 4 — Parser Integration

- [ ] **Oracle text parsing** — Keywords on their own line (e.g., "Flying\nTrample") are handled by the line classifier in `oracle.rs` which calls `Keyword::from_str()`. No parser changes needed if `FromStr` is correct.

- [ ] **`crates/engine/src/parser/oracle_static.rs` — `map_keyword()`**
  This delegates to `Keyword::from_str()`. No changes needed unless the keyword has non-standard Oracle text.

- [ ] **`crates/engine/src/parser/oracle_effect/token.rs` — `parse_token_keyword_clause()`** (if applicable)
  If the keyword appears in token descriptions ("create a 1/1 white Spirit creature token with flying"), ensure it's recognized here.

- [ ] **`crates/engine/src/parser/oracle_effect/token.rs` — `map_token_keyword()`** (if applicable)
  Maps token-description keywords to `Keyword` variants.

### Phase 5 — Synthesis (if applicable)

Some keywords require synthesis in `oracle_loader.rs` — converting the keyword into actual game mechanics that aren't parsed from Oracle text:

- [ ] **`crates/engine/src/database/oracle_loader.rs` — synthesis function**
  Examples:
  - `synthesize_equip()` — converts `Keyword::Equip(cost)` into activated `Effect::Attach` abilities
  - `synthesize_changeling_cda()` — converts `Keyword::Changeling` into a CDA static ability
  - `synthesize_basic_land_mana()` — adds tap-for-mana abilities to basic lands

  If your keyword implies game actions that aren't explicit in Oracle text, add a `synthesize_your_keyword()` function and call it from `build_oracle_face()`.

### Phase 6 — Coverage

- [ ] **`crates/engine/src/game/coverage.rs` — `check_keywords()`**
  `Keyword::Unknown(s)` variants are automatically flagged as unsupported. If your new keyword variant exists but isn't fully implemented, don't leave it as `Unknown` — add the variant to the enum and wire the behavior. `Unknown` should only be for keywords the engine doesn't recognize at all.

### Phase 7 — Tests

- [ ] **`crates/engine/src/types/keywords.rs` — `FromStr` tests**
  Test parsing: `assert_eq!("yourkeyword".parse::<Keyword>().unwrap(), Keyword::YourKeyword);`
  For parameterized: `assert_eq!("yourkeyword:3".parse().unwrap(), Keyword::YourKeyword(3));`

- [ ] **`crates/engine/src/game/keywords.rs` — `has_keyword` tests**
  Test discriminant matching works for your variant.

- [ ] **Runtime behavior tests** in the relevant game module (combat, targeting, etc.)

- [ ] **`cargo test -p engine && cargo clippy --all-targets -- -D warnings`**

---

## Reference: Keyword Categories by Runtime Location

| Runtime Location | Keywords | Check Pattern |
|-----------------|----------|---------------|
| `combat.rs` — blocker legality | Flying, Reach, Fear, Intimidate, Skulk, Shadow, Horsemanship, Menace | `has_keyword()` in `can_block()` |
| `combat.rs` — attacker validation | Defender, Haste (summoning sickness) | `has_keyword()` in `can_attack()` |
| `combat.rs` — attack resolution | Vigilance (skip tap) | `has_keyword()` in `declare_attackers()` |
| `combat_damage.rs` — damage calc | First Strike, Double Strike, Deathtouch, Trample, Lifelink, Wither, Infect | Pattern match in damage assignment |
| `targeting.rs` — target legality | Hexproof, Shroud, Ward | `has_keyword()` in `is_legal_target()` |
| `sba.rs` — state-based actions | Indestructible | `has_keyword()` in `check_sba()` |
| `triggers.rs` — synthetic triggers | Prowess, Undying, Persist, Exalted, Extort | `has_keyword()` + build `ResolvedAbility` |
| `casting.rs` — cast timing/cost | Flash, Convoke, Delve, Improvise | `has_keyword()` in casting validation |

---

## Common Mistakes

| Mistake | Consequence | Fix |
|---------|-------------|-----|
| Adding variant but missing `FromStr` arm | Keyword never parsed from MTGJSON data, silently becomes `Unknown` | Add the string match |
| Missing `keyword_from_tagged()` arm | Existing card-data.json can't deserialize the keyword | Add the JSON match arm |
| Using equality instead of discriminant matching | `Ward({W}) != Ward({2})` — "has ward" check fails for different costs | Use `has_keyword()` which uses `std::mem::discriminant` |
| Adding runtime behavior but no synthesis | Keyword parsed but its implied abilities (like Equip → Attach) never created | Add synthesis function in `oracle_loader.rs` |
| Leaving keyword as `Unknown(String)` with partial support | Coverage report flags it as missing, but it partially works | Add proper enum variant |
| Not testing parameterized parsing | `"ward:{W}"` might fail due to mana cost format | Test both `"Ward:{W}"` and `"Ward:W"` formats |

---

## Self-Maintenance

After completing work using this skill:

1. **Verify references** with the check below
2. **Update keyword category table** if you added runtime behavior in a new location
3. **Update the variant categories** if you added a new parameterization type

### Verification

```bash
rg -q "enum Keyword" crates/engine/src/types/keywords.rs && \
rg -q "fn from_str" crates/engine/src/types/keywords.rs && \
rg -q "fn keyword_from_tagged" crates/engine/src/types/keywords.rs && \
rg -q "fn has_keyword" crates/engine/src/game/keywords.rs && \
rg -q "fn protection_prevents_from" crates/engine/src/game/keywords.rs && \
rg -q "fn parse_keywords" crates/engine/src/game/keywords.rs && \
rg -q "fn synthesize_equip" crates/engine/src/database/oracle_loader.rs && \
rg -q "fn synthesize_changeling_cda" crates/engine/src/database/oracle_loader.rs && \
rg -q "fn check_keywords" crates/engine/src/game/coverage.rs && \
rg -q "fn map_keyword" crates/engine/src/parser/oracle_static.rs && \
rg -q "AddKeyword" crates/engine/src/types/ability.rs && \
echo "✓ add-keyword skill references valid" || \
echo "✗ STALE — update skill references"
```
