# Mana Drain delayed refund (and Mana Sculpt runtime fix)

**Status:** Draft — pending implementation
**Owner:** Engine team
**Related cards:** Mana Drain, Mana Sculpt (any "counter target spell + delayed mana refund" card)
**Related CR:** 603.7 (delayed triggered abilities), 202.3 (mana value), 505.1 (main phase identity), 608.2b (countered spell destination), 106.3 (mana spent to cast)

## Problem

Mana Drain currently parses as `Effect::Counter` followed by `Effect::Unimplemented { name: "at", description: "At the beginning of your next main phase, add an amount of {C} equal to that spell's mana value" }`. The counter portion resolves correctly; the delayed mana refund silently no-ops with a warning log.

The same runtime gap affects Mana Sculpt. Its parse tree looks correct (`Counter` + `sub_ability(CreateDelayedTrigger{ AtNextPhaseForPlayer, Mana{Colorless, ManaSpentToCast{TriggeringSpell, Total}} })`), but at delayed-trigger fire time `state.current_trigger_event` is the firing `PhaseChanged` event, not the original spell-cast event, so `ManaSpentToCast{TriggeringSpell}` resolves to `0`. Mana Sculpt refunds nothing today.

## Goal

Mana Drain and Mana Sculpt both correctly refund colorless mana at the controller's next main phase, in an amount equal to (respectively) the countered spell's mana value or the mana spent to cast it. The fix builds the general class — any future "delayed trigger references a parent-resolution-context value" pattern composes on the same machinery.

## Non-goals

- Other delayed-trigger conditions (`WhenLeavesPlay`, `WhenDies`, `WhenNextEvent`, `WheneverEvent`) — only `AtNextPhaseForPlayer` parsing is touched.
- Quantities embedded inside `TargetFilter` props (e.g. "creature with mana value equal to that spell's mana value" inside a delayed effect). The walker traverses `QuantityExpr` carried directly by `Effect` variants; nested-in-filter quantities are deferred until a card requires them.
- Other "next-phase" delayed triggers that don't depend on parent-resolution context (e.g. "Untap target creature at the beginning of the next end step") — those already work and stay working.
- Frontend / UX changes (target prompt copy, animation, mana pool display).

## Existing infrastructure (reused, not added)

| Building block | Location | Role |
|---|---|---|
| `Effect::CreateDelayedTrigger { condition, effect, uses_tracked_set }` | `types/ability.rs` | Variant that schedules a delayed triggered ability. |
| `DelayedTriggerCondition::AtNextPhaseForPlayer { phase, player }` | `types/ability.rs` | Phase + player-scoped fire condition. |
| `delayed_trigger::resolve` | `game/effects/delayed_trigger.rs` | Existing CreateDelayedTrigger resolver — snapshots parent targets, rewrites placeholder controller, binds tracked sets. |
| `parent_target_snapshot` | `game/effects/delayed_trigger.rs` | Carries the parent ability's `targets` into the delayed `ResolvedAbility`. The countered spell's `ObjectId` is already captured here. |
| `QuantityRef::ObjectManaValue { scope: ObjectScope }` | `types/ability.rs` | Reads a scoped object's `mana_cost.mana_value()`. `Target` scope reads `targets[0]`. |
| `QuantityRef::ManaSpentToCast { scope, metric }` | `types/ability.rs` | Reads payment-time data captured per object. |
| Suffix-position temporal parser | `oracle_effect/mod.rs` | Handles `"... add {C} ... at the beginning of your next main phase"` — emits CreateDelayedTrigger. Used by Mana Sculpt today. |

## Approach

**Approach A — snapshot at CreateDelayedTrigger resolution.**

Add a single walker, `snapshot_parent_dependent_quantities`, that runs inside `delayed_trigger::resolve` AFTER `parent_target_snapshot`. It traverses the inner effect's `QuantityExpr` tree and, for each leaf `QuantityRef` whose meaning depends on parent-resolution context, evaluates it now using the parent ability's targets + current trigger event, then rewrites the leaf to `QuantityExpr::Fixed { value }`. The delayed trigger that gets pushed onto `state.delayed_triggers` holds zero references to parent-resolution context — fire time becomes a self-contained resolution.

One parser arm rounds out the change:
1. **Prefix-position temporal handler** so Mana Drain's word order (`"At the beginning of your next main phase, add {C} ..."`) routes through the same CreateDelayedTrigger path as the existing suffix-position parser.

The parser already handles `"that spell's mana value"` (emits `ObjectManaValue { scope: CostPaidObject }`) — see test `parse_event_context_quantity_spell_mana_value` in `oracle_quantity.rs`. The snapshot walker treats this scope as a snapshottable leaf and resolves it against the parent's `targets[0]` directly.

Mana Sculpt requires no parser change — its existing tree already references a snapshottable quantity (`ManaSpentToCast { TriggeringSpell }`); the new walker fixes it for free.

## Architecture

```text
delayed_trigger::resolve(state, ability, events)
  ├── 1. bind_contextual_filter_to_condition       (existing)
  ├── 2. rewrite AtNextPhaseForPlayer.player        (existing)
  ├── 3. parent_target_snapshot                    (existing — snapshots target IDs)
  ├── 4. ⭐ snapshot_parent_dependent_quantities    (NEW)
  │      walks inner effect's QuantityExpr tree
  │      replaces target-scoped refs with Fixed
  │      uses parent targets + current_trigger_event for resolution
  ├── 5. bind_tracked_set_to_effect                 (existing)
  └── 6. push DelayedTrigger to state.delayed_triggers
```

The walker is the bridge between parent-resolution context (spell on stack, mana value intact, trigger event present) and fire-time context (PhaseChanged event, original spell in graveyard). Its single job is to freeze parent-dependent values into `Fixed` so the inner effect resolves correctly later with no further parent context.

### Snapshottable QuantityRef variants

The walker handles the following leaves; other variants pass through unchanged. **Important context:** the parser anaphors "that spell" / "that creature" to either `ObjectScope::CostPaidObject` (object-property refs like `ObjectManaValue`) or `CastManaObjectScope::TriggeringSpell` (cast-payment refs like `ManaSpentToCast`). Both resolve through `cost_paid_object` and `current_trigger_event` — neither of which is set during a *spell* resolution (Mana Drain and Mana Sculpt are instants, not triggered abilities). The walker therefore reads the parent ability's `targets[0]` **directly** for these refs, bypassing the standard resolver chain for snapshot purposes only. This is an explicit, locally-scoped override; live evaluation of these refs in non-delayed contexts is unchanged.

| QuantityRef | Snapshot source |
|---|---|
| `ObjectManaValue { scope: CostPaidObject }` | `state.objects[parent.targets[0]].mana_cost.mana_value()` (LKI fallback). Anaphor for "that spell's mana value". |
| `ObjectManaValue { scope: Target }` | Same — already resolves against `targets[0]`. Future-proofing if a parser arm emits this scope directly. |
| `ManaSpentToCast { scope: TriggeringSpell, metric }` | Pass `parent.targets[0]`'s ObjectId to `resolve_mana_spent_to_cast_metric` (existing helper in `game/quantity.rs`); applies `metric` against the cast-time snapshot fields on the `GameObject` (`mana_spent_to_cast_amount`, `colors_spent_to_cast`, `mana_spent_source_snapshots`). Anaphor for "the amount of mana spent to cast that spell". |
| `Power { scope: CostPaidObject }` / `Power { scope: Target }` | Same target-resolution pattern. Mirrors `ObjectManaValue`. |
| `Toughness { scope: CostPaidObject }` / `Toughness { scope: Target }` | Same. |

Variants explicitly NOT snapshotted:
- `ObjectManaValue { scope: Source }` / `SelfManaValue` — refer to the ability source object, which persists at fire time.
- `ObjectManaValue { scope: Recipient }` — bound to runtime recipient at fire time, not parent context.
- `Controller` / `Variable` / `Fixed` / aggregate refs (`ObjectCount`, `PlayerCount`) — evaluated against live game state, which is the correct semantic at fire time.

The walker recurses through compound `QuantityExpr` variants (`Offset`, `Multiply`, `DivideRounded`, `Sum`, `Difference`, `UpTo`), snapshotting any nested leaves.

### Why we don't fix the parser's scope choice

It would be tempting to change the parser to emit `Target` (or a new `ParentTargetSpell` scope) for "that spell" when parsing inside a delayed-trigger creation context. We don't, because:

1. The current parser already correctly handles "that spell" in trigger-effect contexts via `CostPaidObject` / `TriggeringSpell`. A context-flag rewire would risk regressing those.
2. The walker's snapshot is a narrow, local override — it only fires at `CreateDelayedTrigger` resolution time, and only replaces leaves with `Fixed` values. The parse tree's scope name is preserved for any other reader.
3. Adding a `ParentTargetSpell` scope or threading a parse-context flag would touch more files and require either new variant wiring across the resolver or a fragile global mutable parser state. The walker is one function in one file.

If a future card needs "that spell" anaphor binding to parent targets in a *non-delayed-trigger* context, we'd revisit and add the explicit scope then.

### Effect-tree traversal

The walker visits every `QuantityExpr`-bearing field on the effect carried by the delayed trigger. Variants of interest for Mana Drain / Mana Sculpt and the immediate adjacent class:

| Effect variant | Quantity field |
|---|---|
| `Effect::Mana { produced: ManaProduction::Colorless { count } }` | `count` |
| `Effect::Mana { produced: ManaProduction::AnyOneColor { count, .. } }` | `count` |
| `Effect::DealDamage { amount, .. }` | `amount` |
| `Effect::Draw { count, .. }` | `count` |
| `Effect::Pump { power, toughness, .. }` | `power`, `toughness` |
| `Effect::PutCounter { count, .. }` | `count` |
| Other amount-bearing variants enumerated by `apply_where_x_effect_expression` | same fields |

To avoid duplication, the walker reuses the variant set already enumerated by `apply_where_x_effect_expression` in `parser/oracle_effect/mod.rs` (which lists every effect that carries a substitutable amount field). Variants outside that set are no-ops for snapshotting.

## Data flow (Mana Drain end-to-end)

```text
1. Cast Mana Drain, target = Lightning Bolt (mana value 1)

2. Mana Drain resolves: ResolvedAbility {
     effect: Counter { target: StackSpell },
     targets: [Object(bolt_id)],
     sub_ability: CreateDelayedTrigger {
       condition: AtNextPhaseForPlayer { PreCombatMain, PlayerId(0) /* placeholder */ },
       effect: Mana { Colorless { count: ObjectManaValue { CostPaidObject } } }
     }
   }

3. Counter::resolve  →  Lightning Bolt removed from stack, moved to graveyard.
   ObjectId is preserved; state.objects[bolt_id] still has mana_cost intact.

4. CreateDelayedTrigger::resolve:
     • parent_target_snapshot  →  delayed.targets = [Object(bolt_id)]
     • placeholder PlayerId(0) →  ability.controller
     • snapshot_parent_dependent_quantities walks effect:
         Mana { Colorless { count: ObjectManaValue{CostPaidObject} } }
       resolves ObjectManaValue{CostPaidObject} using parent.targets[0] = bolt_id:
         state.objects[bolt_id].mana_cost.mana_value() = 1
       rewrites effect:
         Mana { Colorless { count: Fixed{1} } }
     • push DelayedTrigger { effect: Mana{Colorless, Fixed{1}}, condition: AtNextPhaseForPlayer{...} }

5. Turns advance. Mana Drain controller's next PreCombatMain arrives.
   PhaseChanged(PreCombatMain) emitted.

6. check_delayed_triggers fires the trigger:
     • Effect Mana { Colorless { count: Fixed{1} } } resolves with no parent context required.
     • 1 colorless mana added to controller's mana pool.

7. Controller has {C} available for the rest of their main phase. ✅
```

## Data flow (Mana Sculpt)

Same shape as Mana Drain except:
- Parser already emits `ManaSpentToCast { TriggeringSpell, Total }` in the inner effect.
- At step 4, the walker resolves `ManaSpentToCast { TriggeringSpell }` using the parent's `current_trigger_event` (which is still the spell-cast context at delayed-trigger creation), reads the per-object cast snapshot, and bakes it as `Fixed`.
- Steps 5–6 identical.

No parser change for Mana Sculpt. The runtime walker is the single fix point.

## Error handling

| Failure | Behavior | Reasoning |
|---|---|---|
| `ability.targets.is_empty()` for a `Target`-scoped ref | Leave the QuantityRef unmodified (no snapshot) | Walker is opportunistic. At fire time the ref evaluates against empty targets and returns 0 — same as a spell that "fizzles" its target reference. No worse than today. |
| Target ObjectId not present in `state.objects` (already exiled mid-resolution) | Snapshot to `Fixed { value: 0 }` | Engine should fail closed, not panic. Zero refund is the correct unanchored case. |
| `current_trigger_event` is `None` or wrong shape for a `TriggeringSpell` snapshot | Snapshot to `Fixed { value: 0 }` | Same fail-closed reasoning. |

All three are defensive — the worst outcome is "you get 0 mana refunded," never a panic or corrupted state.

## Parser changes

### Change 1 — prefix-position temporal handler

**File:** `crates/engine/src/parser/oracle_effect/mod.rs`

**Where:** Add `parse_prefix_temporal_delayed_trigger` near the top of the imperative-clause dispatcher, before the standard imperative arms.

**Recognizes:** `"At the beginning of [your|the] [next] [phase], <imperative>"`

**Composes with:** the existing `parse_at_beginning_of_phase` combinator (used by the suffix handler) for phase recognition; the standard imperative-parser pipeline for `<imperative>`.

**Emits:** `Effect::CreateDelayedTrigger { condition: AtNextPhaseForPlayer{ <phase>, PlayerId(0) }, effect: <inner>, uses_tracked_set: false }`. The `PlayerId(0)` placeholder is rewritten to the actual controller by the existing resolver step 2.

**Test:** `delayed_trigger_prefix_position_next_main_phase` — a minimal test with an arbitrary inner effect (e.g. `draw a card`) decoupled from any specific card.

### Change 2 — (none — parser already handles "that spell's mana value")

The parser's existing `parse_event_context_quantity` arm emits `ObjectManaValue { scope: CostPaidObject }` for `"that spell's mana value"`, exercised by the existing test `parse_event_context_quantity_spell_mana_value` in `oracle_quantity.rs`. The snapshot walker (Change 3 below) handles this scope explicitly without any parser change.

## Runtime changes

### Change 3 — `snapshot_parent_dependent_quantities` walker

**File:** `crates/engine/src/game/effects/delayed_trigger.rs`

**Signature:**
```rust
fn snapshot_parent_dependent_quantities(
    effect: &mut Effect,
    state: &GameState,
    ability: &ResolvedAbility,
);
```

**Call site:** Inside `delayed_trigger::resolve`, after `parent_target_snapshot` runs and AFTER the delayed `ResolvedAbility` is constructed (so the walker can read its `targets`). The walker mutates the effect held by the delayed `ResolvedAbility`.

**Implementation:**
1. Iterate the effect's quantity-bearing fields (reusing the variant list from `apply_where_x_effect_expression`).
2. For each `QuantityExpr`, call `snapshot_quantity_expr(expr, state, ability)` which recurses through compound variants.
3. At each leaf `QuantityRef`, check if it's in the snapshottable set. If so, resolve it using a synthesized `ResolvedAbility` whose targets are the parent's (already on `ability`); replace the surrounding leaf with `QuantityExpr::Fixed { value }`. Otherwise leave it alone.

**Reuse:** existing `resolve_quantity` in `game/quantity.rs` is the resolver for the snapshot evaluation. No duplicate evaluation logic.

**Tests** (in `delayed_trigger.rs` `#[cfg(test)] mod tests`):
- `snapshot_object_mana_value_target_baked_to_fixed`
- `snapshot_mana_spent_to_cast_triggering_spell_baked_to_fixed`
- `snapshot_no_parent_targets_leaves_ref_intact`
- `snapshot_target_missing_from_objects_baked_to_zero`
- `snapshot_non_snapshottable_ref_passes_through` (e.g. `Source`-scoped refs unchanged)
- `snapshot_compound_expr_recurses` (`Offset { ObjectManaValue{Target}, +1 }` → `Offset { Fixed{N}, +1 }`)

## End-to-end runtime test

**File:** new file `crates/engine/tests/integration/mana_drain_refund.rs` (or extend an existing counter-spell integration test if one exists).

**Setup:**
- Two-player `GameState`.
- Player 0 has Mana Drain in hand and `{U}{U}` in mana pool.
- Player 1 has a 3-cmc spell on the stack (constructed via a minimal cast helper or by direct stack injection — pick whichever the existing counter tests use).

**Steps:**
1. Player 0 casts Mana Drain targeting Player 1's spell.
2. Mana Drain resolves.
3. Assert: countered spell is in Player 1's graveyard.
4. Assert: `state.delayed_triggers.len() == 1`.
5. Assert: the delayed trigger's effect is `Mana { Colorless { count: Fixed{3} } }` (snapshot complete, no live refs).
6. Advance turn(s) until Player 0's PreCombatMain.
7. Assert: `state.delayed_triggers.is_empty()` (one-shot fired and removed).
8. Assert: `state.players[0].mana_pool` contains 3 colorless mana.

This single test proves the parse → resolve → counter → snapshot → advance → fire → mana-in-pool chain.

## Edge cases

| Edge case | Handling |
|---|---|
| X spells (Fireball with X=5) | Snapshot happens BEFORE the spell leaves the stack. `mana_cost.mana_value()` reads the X-bound value (CR 202.3a). ✅ |
| Counter target spell with cost reduction (mana spent < mana value) | Each card's metric is correct: Mana Drain uses `ObjectManaValue` (printed cost), Mana Sculpt uses `ManaSpentToCast` (actual paid). ✅ |
| Replacement effect routes countered spell to exile (Flashback, Harmonize) | `parent_target_snapshot` captures the ObjectId before routing. Snapshot reads from `state.objects` regardless of zone. ✅ |
| Mana Drain controller loses the game before next main phase | Existing delayed-trigger cleanup; the trigger never fires for an eliminated player. ✅ |
| Two Mana Drains in a turn | Each delayed trigger has its own snapshot. APNAP ordering at next main phase. ✅ |
| Future card: "at the beginning of your end step, deal X damage to that creature where X is its power" | Walker handles `DealDamage { amount: Power{Target} }` the same way — generic over Effect variants. ✅ |

## Risks

- **LKI fallback semantics.** The walker reads `state.objects[parent.targets[0]].mana_cost.mana_value()` first, then `state.lki_cache[parent.targets[0]].mana_value` as a fallback (mirroring `SelfManaValue` semantics). If both miss, snapshot is `Fixed{0}`. This is the chosen fail-closed posture; if a future scenario surfaces where LKI is also stale, we revisit.
- **Effect variants we don't touch.** The walker only visits the variant set enumerated by `apply_where_x_effect_expression`. Future effect variants need to be added to that list to receive snapshotting too. This is consistent with how `apply_where_x_effect_expression` already works — adding a new amount-bearing effect requires updating both walkers. Considered an acceptable maintenance cost rather than a fully reflective traversal.
- **Walker scope conservatism.** The walker explicitly handles `CostPaidObject` and `TriggeringSpell` scopes as snapshottable. If a future delayed-trigger effect references parent context through a different scope variant we don't anticipate, the snapshot won't fire and the ref will evaluate to 0 at runtime. The walker has a fixed scope allowlist; expanding it is a deliberate change.

## File touch list

```
crates/engine/src/game/effects/delayed_trigger.rs          (+walker, +unit tests)
crates/engine/src/parser/oracle_effect/mod.rs              (+prefix-position temporal arm, +parser snapshot test)
crates/engine/tests/integration/mana_drain_refund.rs       (NEW, +e2e test)
client/public/card-data.json                               (regenerated by gen-card-data.sh)
```

No new enum variants. No schema changes. No frontend changes. The parser arm for `"that spell's mana value"` already exists and is reused.
