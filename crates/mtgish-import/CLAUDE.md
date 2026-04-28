# mtgish-import — Crate-Specific Guide

This guide is the **first thing any agent working on `crates/mtgish-import/` should read.** It complements the workspace `CLAUDE.md` with rules, patterns, and known-state specific to this crate. If something here conflicts with workspace `CLAUDE.md`, workspace wins — but the items below have been learned the hard way and should not be rediscovered.

## What this crate does

Converts the `mtgish` AST (a friend's hand-crafted Go grammar parser, vendored as `crates/mtgish-import/src/schema/types.rs`) into engine `CardFace`-shaped data. It is the **second source of truth** for card data alongside the native `oracle_nom` parser.

Two purposes:

1. **Replace native output where native fails.** Native parser emits `Effect::Unimplemented` for ~4,500 cards (loud failure) and silently mis-parses an unknown number more (silent failure). Mtgish has full Rules data for ~3,400 of the loud-failure set.
2. **Detect silent failures in the native parser** by structural diff against mtgish output where both produce data.

Architecture is described in `.claude/plans/lovely-dancing-codd.md` (the approved plan) and `src/convert/deferred.rs` (the deferred-gap registry).

## Hard rules — non-negotiable

These have all been violated in past rounds and re-discovered as bugs. Do not re-violate them.

### 1. Strict-failure discipline. No `Effect::Unimplemented` fallbacks.

The converter returns `ConvResult<T> = Result<T, ConversionGap>` everywhere. Every sub-converter (`trigger::convert`, `action::convert`, `cost::convert`, `condition::convert_*`, `filter::convert`, `mana::*`, `quantity::convert`, `replacement::*`, `static_effect::*`, `keyword::try_convert`, `cast_effect::*`, `player_effect::*`, `saga::*`, `companion::*`) propagates `Err(ConversionGap::*)` upward. The whole card fails if any rule fails.

**Never** add an `unwrap_or(Effect::Unimplemented)`, `let _ = result;` swallow, or `match result { Err(_) => Effect::Unimplemented, ... }`. The strict-failure type discipline is what makes this crate's output trustworthy. The native parser already emits `Unimplemented` — that channel exists and is sufficient.

### 2. Sub-converter Err MUST be reported via `ctx.unsupported(path)`.

This was the bug that hid 28,871 silent drops for seven rounds. **Every sub-converter that returns `Err` must record the gap to `ctx` before propagating.** Specifically:

- `convert_card` in `mod.rs` instruments every rule-level error via `enrich_gap_path` → `ctx.unsupported(...)`. If you add a new top-level call site, mirror that pattern.
- Sub-converter signatures that return `ConvResult<T>` propagate without ctx access; their gaps are recorded by the **caller's enrichment wrapper**. Do not add a second recording inside the sub-converter (it produces double-counting).
- The single `ctx.unsupported()` call sites live in `mod.rs::convert_rule`. If you add another, audit `report.cards_with_unsupported` semantics — it's a per-card flag, not a per-gap counter, but `report.unsupported{path}` is per-occurrence.

Path format: `<face>/Rules[<idx>]/Rule::<TopVariant>::<InnerVariant>::...` — concrete enough to surface specific blockers, not just top-level Rule kinds.

### 3. No condition drops to `None` "as a permissive fallback."

This was the round-3 `CastEffect` bug: `RequiresCondition { condition: None }` evaluates as **always-pass** in the engine runtime (`is_none_or` semantics in `restrictions.rs:494`). Dropping the inner condition from `CantBeCastUnless` / `CantBeCastIf` / `MayCastWithoutPayingIf` / `MayCastAsThoughItHadFlashIf` / `AlternateCastingCostIf` (and `Rule::FlashForCasters`) converts "can't be cast unless X" into "can always be cast" — a category-(c) rules-correctness violation. All six arms strict-fail with `EnginePrerequisiteMissing { engine_type: "ParsedCondition" }` today.

Rule: **if you can't translate the condition, strict-fail the entire rule.** Better to leave a card un-converted than to ship subtly wrong card data. The native parser is doing the same thing today and that's part of what we're trying to detect.

When the `mtgish::Condition → engine::ParsedCondition` bridge exists, those gaps will close. Until then, strict-fail.

### 4. Engine extensions — protocol.

Adding a new variant to `engine::types::ability::*`, `engine::types::keywords::*`, `engine::types::statics::*`, `engine::types::replacements::*`, etc. is allowed when:

- The variant covers a **category** of cards, not one card. Build for the class.
- It's not redundant with an existing primitive. Search before extending. Past mistake to avoid: don't add `DamageModification::Foo` when `DamageModification::Bar` + a parameter already covers it.
- It carries a **CR annotation grep-verified against `docs/MagicCompRules.txt`** before commit. Fabricated CR numbers are worse than no annotation. If a keyword is genuinely absent from the rules text (e.g., recent un-set mechanics), annotate `// CR ???: needs manual verification (not in CR text)` — that's honest.
- All exhaustive `match` statements in the engine that the new variant breaks are extended. Use `cargo check -p engine` to find them. **No wildcard fallback arms** to silence the compiler.
- A runtime stub in the engine is acceptable (this crate widens types ahead of runtime), but the stub must be **explicitly marked**: `// RUNTIME: TODO — converter accepts this; engine handler is a no-op stub. CR <X>` on the variant doc-comment.

Each engine extension ships in **one commit** paired with the converter arm that uses it. Don't batch unrelated engine extensions.

### 5. No bool fields. Ever.

Per workspace `CLAUDE.md`. This crate has not added a bool field yet — keep it that way. Use existing typed enums (`ControllerRef`, `Comparator`, `Option<T>`, `PlayerFilter`, `TargetFilter`, `Zone`, etc.) or introduce a typed enum for the new dimension.

### 6. Reuse engine primitives before adding them.

**ALMOST EVERYTHING ALREADY EXISTS in the engine.** The native `oracle_nom` parser has been mapping Oracle text to engine types for months. For practically any concept mtgish encodes, there is already a typed slot — sometimes under a different name, sometimes parameterized differently than mtgish encodes it. Your job is **exhaustive engine spelunking**, not extension.

#### Mandatory spelunking protocol

Before considering ANY new engine variant, run all five searches:

1. `rg -n '<concept_keyword>' crates/engine/src/types/` — search by name
2. `rg -n '<inverse_concept>' crates/engine/src/types/` — search for the inverse (e.g., "untapped" finds Tap with `negated: true`)
3. `rg -n 'negated|invert|polar' crates/engine/src/types/ability.rs` — find inversion-parameter variants (the engine has at least 7 condition-bearing variants with `negated: bool` fields)
4. **Read `crates/engine/src/parser/oracle_*.rs` for how the native parser maps the same concept** — this is the **reverse dictionary** that turns "the concept I'm trying to express" into "the engine type that expresses it"
5. Read `crates/engine/src/database/synthesis.rs` for synthesized keyword/trigger output

Only after all five searches return nothing for the concept AND its inverses AND its parameterizations should you consider an engine extension — and even then, **defer to a separate round** and ship a strict-failing converter arm with `EnginePrerequisiteMissing { engine_type, needed_variant }` documenting the proposed shape.

#### The two inversion mechanisms

The engine expresses "not / unless" patterns two ways. Use whichever the inner condition's slot supports:

1. **Wrapper form** — `TargetFilter::Not { filter }` (line ~1615), `StaticCondition::Not { condition }` (line ~2276). Add the missing sibling `AbilityCondition::Not` only if exhaustive verification confirms it doesn't exist (verify on HEAD via `git show HEAD:crates/engine/src/types/ability.rs`, not buffer state).
2. **Parameter form** — `negated: bool` field on existing condition variants. Example: `SourceIsTapped { negated: bool }` ("When `negated` is true, the condition is met when the source is *untapped*"). Grep `negated: bool` in `ability.rs` to see the catalog.

When converting `Action::Unless(cond, body)`, prefer the parameter form if the inner condition has one; otherwise use the wrapper form.

#### Catalog of existing primitives that frequently come up

- `TargetFilter` / `FilterProp` (29 + 60 variants) — most filter shapes already exist. Includes `TargetFilter::Not` for inversion.
- `QuantityExpr` / `QuantityRef` (64 variants) — most counts/values already expressible. **Use `QuantityRef::Variable { name: "X" }` for X-cost/X-quantity, never `Fixed { value: 0 }`.**
- `ManaCost` — full cost grammar.
- `AbilityCondition` / `TriggerCondition` / `StaticCondition` — three condition contexts; each is a separate enum, don't reuse arms across.
- `ReplacementDefinition` fields: `damage_modification`, `damage_source_filter`, `damage_target_filter`, `combat_scope`, `quantity_modification`, `redirect_target`, `valid_card`, `valid_player`, `destination_zone`, `mana_modification`, `additional_token_spec`, `ensure_token_specs`, `shield_kind`, `is_consumed`.
- `TriggerDefinition` fields: `damage_kind: DamageKindFilter` (CombatOnly/NoncombatOnly), `combat_scope: Option<CombatDamageScope>`, `valid_player`, `valid_card`, `destination_zone`. Combat-damage triggers populate these on the existing `DamageDone` mode — don't add new variants.
- `AbilityDefinition::player_scope: Option<PlayerFilter>` — exists for non-You PlayerAction scoping. Pure converter threading.
- `StaticMode::ReduceCost { amount, spell_filter, dynamic_count }` / `RaiseCost` — cost-modification statics.
- `SpellCastingOption` family — alternative-cost / additional-cost / as-though-flash framework.
- `CastingRestriction::RequiresCondition { condition }` — gated casting. **`condition: None` evaluates as ALWAYS-PASS** via `is_none_or` in `restrictions.rs:494` — never drop the condition to None as a "permissive fallback."

When in doubt, grep the engine first. When still in doubt, read the native parser.

### 6b. The orchestrator violates reuse-first too.

The agent who wrote this guide watched the orchestrator (the user-facing Claude Code) direct `AbilityCondition::Not`, `DamageModification::PreventAll`, and `Keyword::Offering` extensions across multiple rounds — all either redundant with existing primitives or added without proper verification. Two patterns to internalize:

1. **When a directive says "add engine variant X," verify X doesn't already exist before complying.** Run the 5-grep protocol on HEAD (`git show HEAD:<path>` if buffer state is suspect — the orchestrator has confused in-flight edits with committed code more than once).
2. **Push back with evidence when the directive is wrong.** Cite `file:line` and grep output. Trust the directive's *intent* (the user wants coverage), not its *literal premise* ("add this variant"). The agent is encouraged — required, even — to refuse a duplicate-extension directive and ship the reuse-based equivalent instead.

The orchestrator is fallible. The engine grep is authoritative.

### 7. CR annotations on every rules-implementing arm.

Workspace `CLAUDE.md` is explicit: any code implementing a CR rule carries a verified CR annotation. **Verify by grepping `docs/MagicCompRules.txt` before committing.** Do not trust memory or training data — the 701.x / 702.x numbers are arbitrary sequential assignments and very prone to hallucination.

The rules text was downloaded via `./scripts/fetch-comp-rules.sh` and lives at `docs/MagicCompRules.txt` (gitignored).

### 8. Audit verdicts go through the parameterization filter.

Audits frequently identify "engine extension required: add variant X." Before complying, route the verdict through this filter:

1. **Verify the slot really doesn't exist** via the 5-grep protocol from rule §6.
2. **Apply the parameterization filter** (workspace `CLAUDE.md` "Parameterize, don't proliferate"): is variant X a sibling to existing variants that share a structural axis? If yes, the right response is **REFACTOR THE PARAMETERIZATION**, not add the sibling. Common smells in this codebase:
   - `LifeTotal` / `OpponentLifeTotal` / `TargetLifeTotal` → consolidate via `LifeTotal { player: PlayerScope }`
   - `HandSize` / `OpponentHandSize` → consolidate via `HandSize { player: PlayerScope }`
   - `UnlessControlsCountMatching` / `UnlessControlsMatching` / `UnlessControlsOtherLeq` → consolidate via `UnlessQuantity { comparator, filter, count: QuantityExpr }`
   - `WhenLeavesPlay` / `WhenLeavesPlayFiltered` / `WhenDies` / `WhenDiesOrExiled` → consolidate via `WhenZoneChange { destination, source_filter }`
3. **Categorical boundary check.** Any proposed parameterization axis MUST lie within a single CR rule section. Cross-section unification belongs at `TargetFilter` (cross-subject flexibility) or at the effect handler (cross-section uniform behavior — e.g., `Effect::DealDamage` per CR 120 unifies player/creature/planeswalker damage at the effect layer), NEVER at the leaf-reference layer (`QuantityRef` / `FilterProp` / `ReplacementCondition`).
4. **Sequencing.** Parameterization refactor rounds ship BEFORE coverage rounds that would add new sibling variants to the parameterized family. Adding sibling variants while a parameterization is pending compounds the eventual refactor cost. The strict-failure tag in this crate's converter is the right place to leave the gap visible — coverage waits, architecture wins.
5. **Explicit gate.** Run the workspace `add-engine-variant` skill BEFORE proposing or implementing any engine variant addition. The skill is the runnable checklist; this rule is the policy.

This rule prevents the "audit grep-by-name → propose sibling variant → extend engine → cement debt" pipeline that would otherwise compound across rounds. Past instances this would have caught: `LifeTotalAggregate` proposal (Round CC, would have added a 5th LifeTotal sibling), `PartySize` proposal (Round DD, would have added another scope-leaf to a family that needs `PlayerScope` parameterization), `CreateTriggerUntil` proposal as engine extension (mapped cleanly to existing `Effect::CreateDelayedTrigger`).

## Crate structure (current)

```
src/
├── lib.rs
├── schema/
│   ├── mod.rs
│   └── types.rs              ← vendored mtg_types.rs (mechanically edited per plan §Schema vendoring)
├── convert/
│   ├── mod.rs                ← top-level dispatch + per-card driver
│   ├── result.rs             ← ConversionGap, ConvResult, report_path
│   ├── trigger.rs            ← Trigger → TriggerCondition + filter + zone
│   ├── action.rs             ← Action / ActionList → Effect (heaviest module — see §Top remaining work)
│   ├── cost.rs               ← Cost → AbilityCost
│   ├── filter.rs             ← Permanents/Permanent/Cards/Spells → TargetFilter
│   ├── condition.rs          ← Condition → TriggerCondition / AbilityCondition / StaticCondition
│   ├── quantity.rs           ← GameNumber → QuantityExpr / QuantityRef
│   ├── mana.rs               ← ManaSymbolX[] → ManaCost; cost-reduction conversions
│   ├── keyword.rs            ← vanilla & parameterized keywords
│   ├── replacement.rs        ← AsPermanentEnters / Replace* → ReplacementDefinition
│   ├── static_effect.rs      ← StaticLayerEffect → ContinuousModification
│   ├── cast_effect.rs        ← CastEffect family → casting metadata
│   ├── player_effect.rs      ← PlayerEffect / EachPlayerEffect → player-scope statics
│   ├── saga.rs               ← SagaChapters → saga static + chapter triggers
│   ├── companion.rs          ← Companion → CompanionCondition
│   └── deferred.rs           ← documentation: deferred-gap registry (no executable code)
├── report.rs                 ← ImportReport, Ctx, gap recording
├── provenance.rs             ← (planned) breadcrumb tracker
└── bin/
    ├── convert.rs            ← cards.json → mtgish-import-report.json
    └── debug_deser.rs        ← deserialization smoke test
```

## Test discipline

Two-tier golden snapshots per the plan:

- **Structural goldens** (`tests/golden/structural/`): semantic content. **Re-blessing requires manual review** — no `BLESS_STRUCTURAL=1` env var. Catch real bugs.
- **Shape goldens** (`tests/golden/shape/`): serialization shape only. `BLESS_GOLDEN=1` re-bless allowed for engine type schema evolution.

A converter test classifies its own goldens. Idiom converters (cast effects, replacements, sagas) ship structural goldens. Pure leaf converters (mana, single keyword) ship shape goldens.

Every coverage commit must keep all goldens passing. If a structural golden changes, re-review the diff manually before re-blessing.

## Top remaining work (as of round 7 instrumentation)

The reporting bug fix (commit `db840ca41`) exposed the actual gap distribution. Top blockers:

1. **`action::convert_list` shape coverage** (~9,000+ cards). Currently only handles `Actions::ActionList`. Modal/Targeted/Repeated/ActionForEachPlayer/etc. all strict-fail. Highest single ROI.
2. **`Action::AddMana`** (~1,500+ cards). Mana-producing activated abilities — no current arm.
3. **`Action::CreateTokens`** (~900+ cards).
4. **`Action::Loyalty`** (~760+ cards). Planeswalker activation costs.
5. **`Action::CreatePermanentLayerEffectUntil`** (~550+ cards). End-of-turn pump and ability grants.
6. **`Action::MayCost / MayAction`** (~800+ cards). Optional-payment / optional-action wrappers.
7. **`filter::convert(Permanents)` long tail** (~850+ cards across trigger/static dispatch).

Closing the top three buckets alone would unlock ~12,000 cards via the strict-failure interlock. The user's 20k+ clean ceiling is plausible.

**Earlier rounds (5-7) drained the deferred registry, which surfaced gaps from the 339-card explicitly-recorded set — a 1.1% sample of the real distribution.** Future coverage work should be driven by `data/mtgish-import-report.json`'s top-30, not by the deferred-registry list.

## Engine-extension audit (rounds 2-7 self-flag)

- ~~**`DamageModification::PreventAll`** (round 3)~~ — **REMOVED**. Was a doc-comment-confessed alias of `Minus { value: u32::MAX }` (saturating-subtraction yields 0). Continuous-vs-consumed semantics are still distinct from `ShieldKind::Prevention { All }`, but that distinction lives in *which container the modification is housed in*, not in a separate variant. Converter now emits `Minus { value: u32::MAX }`.
- **`Keyword::Offering(String)`** (round 6): confirmed structurally distinct from `Keyword::Champion(String)` — Offering is sacrifice-as-cost, Champion is exile-as-cost. ✅
- **`Keyword::{Replicate, Awaken, ForMirrodin, MoreThanMeetsTheEye, Freerunning, Increment, Specialize, Impending}`**: status varies — some have engine runtime via `database/synthesis.rs`, others are type-only stubs. **Each variant should have a `// RUNTIME:` doc-comment naming its handler or marking it stub.** This audit is incomplete; a fresh reviewer should sweep.
- **CR annotations on round-4 keywords** were back-filled in commit `4603f7b21`. `Keyword::Specialize` carries the honest `CR ???: not in CR text` flag.

## Concurrency contract

Per the plan §Concurrency Contract:

- Engine extensions ship in **separate PRs first**, lands on main, then the converter PR depending on it.
- Engine files outside this crate are read-only **except for explicit, documented engine extensions paired with converter arms**.
- Daily rebase against main if a converter PR is open >1 day.
- Use `Edit` not `Write` on engine files. Multi-agent safety.

## How to make progress

1. Read the current report: `cargo run --release -p mtgish-import --bin mtgish-convert -- data/mtgish-cards.json /tmp/r.json && head -40 /tmp/r.json`. The top-30 unsupported variants are the work queue. (`data/mtgish-cards.json` is the checked-in snapshot of the upstream `mtgish` build's `cards.json` output — only the JSON is committed; the upstream parser sources are not redistributed in this repo.)
2. Pick the highest-frequency blocker. Read the relevant engine type to understand existing primitives. Reuse-first.
3. Write the converter arm with a verified CR annotation. Keep all goldens passing.
4. `cargo check -p mtgish-import` for fast iteration; `cargo build --release -p mtgish-import` + run the binary to measure clean-count delta before committing.
5. One commit per logical unit. Update `convert/deferred.rs` if a deferred bucket closes.
6. **Never bypass strict-failure. Never drop conditions to `None`. Never add bool fields. Always verify CR.**

## Lessons from prior rounds (do not repeat)

- The reporting layer is part of the contract. If you add a new failure path, make sure the gap is recorded.
- "Type-only stub" framing requires actual verification. Grep the engine for the variant before claiming it has no runtime.
- The deferred-registry strikethroughs are progress markers, not coverage metrics. Real coverage comes from the report's top-N.
- Compounding agent context is an asset for forward construction, a liability for retrospective audit. Spawn fresh agents for review work.
