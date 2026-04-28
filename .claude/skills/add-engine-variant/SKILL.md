---
name: add-engine-variant
description: Runnable checklist gate for any proposed engine enum variant addition. Routes the proposal through the parameterization filter, the categorical-boundary check, and the existence verification before extension is permitted. Invoke this skill BEFORE proposing or implementing any new variant on QuantityRef, QuantityExpr, FilterProp, TargetFilter, ReplacementCondition, AbilityCondition, TriggerCondition, StaticCondition, ChoiceType, DelayedTriggerCondition, Effect, Keyword, ContinuousModification, or any other engine enum.
---

# add-engine-variant — Engine Enum Extension Gate

## When to invoke

This skill is the gate for any work that would add a variant to an existing engine enum. Run it BEFORE:

- Proposing an engine extension in a plan or task description
- Drafting an engine PR that adds a variant
- Approving an audit verdict of "engine extension required"
- Wiring a converter arm to a not-yet-existing variant

If you find yourself about to type `pub enum Foo { ... NewVariant ... }` in `crates/engine/src/`, pause and run this skill.

## The three filter stages — all must pass

### Stage 1: Existence verification (5-grep protocol)

The variant might already exist under a different engine-native name. The mtgish AST and the engine vocabulary are not 1:1; the engine often has the concept under a different name.

Run **all five searches** before considering extension:

```bash
# 1. Direct name search
rg -n "<concept_keyword>" crates/engine/src/types/

# 2. Inverse concept (engine often expresses "not X" as "X { negated: bool }" or "X { negated: true }")
rg -n "<inverse_concept>" crates/engine/src/types/

# 3. Inversion-parameter variants
rg -n "negated: bool|invert|polar" crates/engine/src/types/ability.rs

# 4. Native parser reverse dictionary — how does oracle_nom express this concept?
rg -n "<concept_keyword>|<engine_concept>" crates/engine/src/parser/oracle_*.rs

# 5. Synthesis layer
rg -n "<concept_keyword>" crates/engine/src/database/synthesis.rs
```

**Stage 1 verdicts:**

- **EXISTS_SAME_NAME**: variant exists. Stop. Wire to it.
- **EXISTS_DIFFERENT_NAME**: concept exists under engine-native name (e.g., mtgish `CreateTriggerUntil` ↔ engine `Effect::CreateDelayedTrigger`). Stop. Map mtgish AST to the existing slot.
- **EXISTS_AS_PARAMETER**: concept exists as a parameter value of a more general variant (e.g., "untapped" exists as `Tap { negated: true }`). Stop. Use the parameter form.
- **DOES_NOT_EXIST**: proceed to Stage 2.

If the audit you're acting on said "extension required" and Stage 1 returns EXISTS_*, the audit is wrong. Cite the file:line of the existing slot and refuse the extension.

### Stage 2: Parameterization filter

If the slot truly doesn't exist, the next question is: *should it exist as a sibling, or should the parent enum be parameterized so the new concept becomes a parameter value of an existing variant?*

For each candidate enum, look for the **sibling-cluster smell**:

- Three or more variants that share a name root (`X` / `OpponentX` / `TargetX` / `AllX`)
- Three or more variants that differ only in a context label (`UnlessControlsCountMatching` / `UnlessControlsMatching` / `UnlessControlsOtherLeq` — the axes are scope and comparator)
- Three or more variants that differ only in a comparator/aggregator/scope dimension
- Three or more variants where the structural differences could be expressed as a `(op, scope, target)` tuple

If you find one, **the proposed variant is almost certainly a parameterization gap, not a missing sibling.** Compare:

| Smell | Refactor target |
|---|---|
| `LifeTotal` / `OpponentLifeTotal` / `TargetLifeTotal` (+ proposed `LifeTotalAggregate`) | `LifeTotal { player: PlayerScope }` where `PlayerScope` is `Controller \| Target \| Opponent { aggregate } \| AllPlayers { aggregate }` |
| `HandSize` / `OpponentHandSize` (+ proposed `TargetHandSize`) | `HandSize { player: PlayerScope }` |
| `UnlessControlsCountMatching` / `UnlessControlsMatching` / `UnlessControlsOtherLeq` | `UnlessQuantity { comparator: Comparator, filter: TargetFilter, count: QuantityExpr }` |
| `WhenLeavesPlay` / `WhenLeavesPlayFiltered` / `WhenDies` / `WhenDiesOrExiled` | `WhenZoneChange { destination: ZoneFilter, source_filter: TargetFilter }` |

**Stage 2 verdicts:**

- **REFACTOR_FIRST**: sibling-cluster smell detected. Open a parameterization-refactor round (Round Π-N) that consolidates the existing siblings AND absorbs the new concept as a parameter value. Do NOT add the proposed variant. The strict-failure tag is the right place to leave the gap visible while the refactor is pending.
- **EXTEND_OK**: no sibling-cluster smell — the proposed variant is genuinely orthogonal to existing variants. Proceed to Stage 3.

The compounding-cost rule: one new sibling has near-zero cost; ten siblings make the eventual refactor multi-week as call sites multiply across parser, converter, resolver, and tests.

### Stage 3: Categorical-boundary check

If you've decided extension is appropriate (either as a new variant or as a parameter on an existing one), the proposed parameterization axis MUST lie within a single CR rule section.

Cross-section unification belongs at:

- `TargetFilter` (already enumerates any subject type — players, creatures, planeswalkers, battles, etc.)
- The effect handler (where rules unify behavior — `Effect::DealDamage` per CR 120 handles all damage subjects uniformly)

NOT at:

- `QuantityRef` / `FilterProp` / `ReplacementCondition` / similar leaf-reference layers

**Examples of category errors to refuse:**

- `Life { target: {Self, Opponent, Creature}, type: {Total, Remaining} }` — conflates CR 119 (player life) with CR 120 (damage marked) and CR 209 (toughness). Three rule sections, three different runtime resolvers. Refuse.
- `ZoneCount { subject: {Player, Creature, Spell}, zone: ZoneRef }` — subjects belong to different scopes (game-wide for Player, battlefield for Creature, stack for Spell). The unification belongs at `TargetFilter`, not here. Refuse.

**Stage 3 verdicts:**

- **WITHIN_SECTION**: parameterization axis is within a single CR rule section. Extension is approved. Proceed to implementation.
- **CROSSES_SECTIONS**: refuse the parameterization. Push the unification to `TargetFilter` or the effect handler. Re-design.

## After all three stages pass

If you've made it through all three stages with EXTEND_OK / WITHIN_SECTION verdicts:

1. **Verify CR annotation via grep.** Every new variant carries a CR number, and every CR number is verified by grepping `docs/MagicCompRules.txt` BEFORE the annotation is committed. The 701.x and 702.x ranges are arbitrary sequential assignments and are especially prone to hallucination — do not trust memory.

2. **Update all exhaustive `match` statements.** Use `cargo check -p engine` to find them. NO wildcard fallback arms to silence the compiler — those mask future variant additions.

3. **Document runtime status.** If the variant is type-only (no runtime handler yet), add `// RUNTIME: TODO — converter accepts this; engine handler is a no-op stub. CR <X>` on the variant doc-comment. Type-only stubs are acceptable; silent runtime stubs are not.

4. **Pair with converter arm in one commit.** Engine extensions ship with the converter arm that uses them, in a single coherent commit. Don't batch unrelated engine extensions.

5. **Concurrency contract.** Engine extensions ship in a separate PR before the converter PR depending on them, OR in one paired commit if the work is done by a single agent. No half-extensions.

## Anti-patterns this skill prevents

- "Audit said add variant X" → adding sibling without verification (the audit is wrong about a third of the time per session metrics)
- "Engine doesn't have it" → without grepping for engine-native names (CreateTriggerUntil mtgish ↔ CreateDelayedTrigger engine)
- "Just one more sibling" → ignoring the sibling-cluster smell and compounding parameterization debt
- "Unify under one type" → crossing CR rule sections and conflating runtime resolvers
- "Type-only stub returning true" → like `CastViaKicker { .. } => true` which silently mis-resolves at runtime

## When refusing

If this skill returns REFUSE_WITH_REFACTOR or REFUSE_WITH_FILTER_LIFT, your output to the orchestrator should include:

1. The exact stage that failed (Stage 1: EXISTS_*, Stage 2: REFACTOR_FIRST, Stage 3: CROSSES_SECTIONS)
2. The evidence (file:line citations, grep output, sibling cluster identification)
3. The recommended alternative (the existing slot to use, the refactor round to open, or the layer where unification belongs)
4. The cards/coverage being deferred and the strict-fail tag to leave in place

Refusing is the correct outcome when any stage fails. Coverage waits, architecture wins.

## Related artifacts

- Workspace `CLAUDE.md` — "Parameterize, don't proliferate" principle (the policy this skill enforces)
- Crate `mtgish-import/CLAUDE.md` Rule §8 — audit-verdict filter discipline
- Memory note `feedback_parameterize_dont_proliferate.md` — user directive recording this as a hard rule

## Inputs / outputs

**Invocation:** describe the proposed extension — the enum, the variant, the audit or task that proposed it, the cards it would unlock.

**Output:** one of:
- `APPROVED: <enum>::<variant> { <fields> }` with CR annotation, runtime-status doc-comment, and ready-to-implement signature
- `REFUSE_WITH_EXISTING_SLOT: use <engine_path>:<line>` with the wiring to use instead
- `REFUSE_WITH_REFACTOR: open Round Π-N consolidating <sibling_cluster>` with the proposed parameterized form
- `REFUSE_WITH_FILTER_LIFT: push to <TargetFilter | effect_handler>` with the cross-section unification target

The output is binding. If APPROVED, proceed. If any REFUSE_*, the extension does not ship until the refusal's recommended alternative is taken.
