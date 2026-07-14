---
name: add-ai-feature-policy
description: Use when adding a new deck-aware AI feature (`DeckFeatures` axis) and its companion policies (`TacticalPolicy` + optional `MulliganPolicy`) to `phase-ai`. Codifies the structural-detection pattern, AST-type locations, parts-based predicate convention, registry wiring sites, and recurring review traps so a new feature ships in a single commit without re-research.
---

# Adding a Deck-Aware AI Feature + Policy

This is the authoritative checklist for adding a new feature to the three-layer AI architecture (Features → Plan → Policies). Nine features exist as reference: `landfall`, `mana_ramp`, `tribal`, `control`, `aristocrats`, `aggro_pressure`, `tokens_wide`, `plus_one_counters`, `spellslinger_prowess`. Each follows the same pattern; mirror it.

**Before you start:** Read `landfall.rs` (simplest), then `aristocrats.rs` (geometric-mean commitment with identity lookup), then one feature similar in shape to yours. Trace from `features/<name>.rs::detect()` through `DeckFeatures::analyze` and `AiSession::from_game` through `policies/<name>.rs::verdict` so the data flow is concrete.

> **CR Verification Rule.** Every CR number in annotations MUST be verified by grepping `docs/MagicCompRules.txt` BEFORE you write it. Do NOT rely on memory or prior prompts. The 701.x and 702.x sections are arbitrary sequential assignments that hallucinate consistently. Run `grep -n "^701.34" docs/MagicCompRules.txt` for every number. CR 122.1d is Stun (NOT +1/+1 — that's CR 122.1a). CR 701.27 is Transform (NOT Proliferate — that's CR 701.34). Two real catches from the existing batch — you will have similar misses unless you grep.

---

## 1. Architecture (memorize this)

```
AiSession (per-game cache; built once in AiSession::from_game)
  ├─ features: HashMap<PlayerId, DeckFeatures>    ← Layer 1: structural detection
  ├─ plan:     HashMap<PlayerId, PlanSnapshot>    ← Layer 2: bottoming + curve schedule
  ├─ deck_profile / strategy: per-player strategic profiles
  ├─ synergy:  HashMap<PlayerId, SynergyGraph>
  └─ memory:   PolicyMemory

DeckFeatures
  ├─ landfall:       LandfallFeature
  ├─ mana_ramp:      ManaRampFeature
  ├─ tribal:         TribalFeature
  ├─ control:        ControlFeature
  ├─ aristocrats:    AristocratsFeature
  ├─ aggro_pressure: AggroPressureFeature
  ├─ tokens_wide:    TokensWideFeature
  ├─ plus_one_counters: PlusOneCountersFeature
  ├─ spellslinger_prowess: SpellslingerProwessFeature
  └─ <your new feature>

Policies (Layer 3)
  ├─ TacticalPolicy — in-game decisions (CastSpell, ActivateAbility, DeclareAttackers, ...)
  └─ MulliganPolicy — pre-game hand evaluation (sibling trait)

Both run inside their respective registries (`PolicyRegistry::default`,
`MulliganRegistry::default`). Activation is the single multiplicative knob.
```

`AiSession::from_game` analyzes `current_main` plus `current_commander`; commander entries are
weighted as build-around cards before feature/profile/synergy detection. One-off analysis helpers
that only receive a deck slice stay pure over that slice.

**Single-knob rule.** A `TacticalPolicy` exposes `activation()` returning `Option<f32>`. The registry multiplies `verdict.delta * activation` exactly once. There is no `archetype_scale`, no `turn_mult`, no second pass. If a policy needs archetype/turn-sensitive weight, it computes it inside `activation()` from the inputs.

**Score contract.** Tactical policy deltas are card-equivalent units: `delta = 1.0` means one card of expected value. Use the band helpers on `PolicyVerdict` so the chosen strength is visible at the call site:

| Band | Helper | Range | Meaning |
|---|---|---|---|
| Nudge | `PolicyVerdict::nudge` | `(0.0, 0.3]` | tie-breaker |
| Preference | `PolicyVerdict::preference` | `(0.3, 1.5]` | half-card to 1.5-card preference |
| Strong | `PolicyVerdict::strong` | `(1.5, 5.0]` | multi-card swing |
| Critical | `PolicyVerdict::critical` | `(5.0, 15.0]` | game-deciding |
| Veto | `PolicyVerdict::reject` | n/a | never take this action |

All scoring scalars come from `AiConfig::policy_penalties` / `PolicyPenalties` with rationale comments. Thresholds that only gate activation stay as `pub const` in the policy/feature module. Never encode vetoes as sentinel scores.

---

## 2. Hard constraints (non-negotiable)

| Rule | Why | How to apply |
|---|---|---|
| **Structural detection only.** No card-name matching for classification. | Name lists rot; structural patterns generalize across sets. | Walk `CardFace.{abilities,triggers,static_abilities,replacements,keywords,card_type}`. Use `crate::ability_chain::collect_chain_effects` for ability chains. |
| **Identity lookup is exempt.** `Vec<String>::iter().any(\|n\| n == &obj.name)` is allowed when the policy needs to re-find a structurally-classified card on the battlefield or in hand. | Mulligan policies see `GameObject` shells whose abilities aren't resolved yet. | Populate `payoff_names: Vec<String>` once per UNIQUE face in `detect()`, NOT per playset copy. The `no_name_matching` lint at `features/tests/no_name_matching.rs` enforces the forbidden-pattern list and exempts identity lookup. |
| **CR annotations grep-verified.** Every CR comment must match `docs/MagicCompRules.txt`. | Hallucinated CR numbers create false confidence. | Run `grep -n "^XXX.Y" docs/MagicCompRules.txt` for every annotation BEFORE writing. Don't annotate plumbing/utility code. |
| **Single-authority cost rule.** Always `ability.cost_categories()`, never destructure `AbilityCost`. | Scattered cost handling means new cost types break in a dozen places. | When you need "this ability sacrifices a permanent", use `cost_categories().contains(&CostCategory::SacrificesPermanent)`. |
| **No bool fields.** Use existing typed enums (`ControllerRef`, `Comparator`, `TurnOrder`, etc.). | Booleans aren't composable; an enum is self-documenting. | If you need a "your-vs-opponent" distinction, use `ControllerRef::You/Opponent`. If you need on-play vs on-draw, use the existing `TurnOrder`. |
| **Use `crate::ability_chain::collect_chain_effects`.** | Single source of truth for ability-chain walking. | Never re-implement `collect_chain_effects`. If you need to walk a `ResolvedAbility` chain in a policy, use `policies/context.rs::collect_ability_effects` (handles `sub_ability` chain). |
| **No `git add -A` or `git add .`.** | Pre-existing untracked files at the repo root (e.g., `parser-batch-plan.md`) get accidentally committed. | Use explicit `git add path1 path2 ...`. |
| **No unguarded board-wide / affordability engine calls in `activation()`/`verdict()`.** | Policies run per-candidate at every AI search node; a board-wide sweep there is a wall-clock landmine that `cargo ai-gate` cannot see. | Order predicates cheap-to-expensive: card-local AST checks first, `max_x_value` / `find_legal_targets` / mana sweeps last and only once the class is confirmed. See the **Performance** section. |

---

## Performance — `verdict()` runs in the AI search inner loop

`PolicyRegistry::verdicts()` (`registry.rs:375-433`) runs **every** policy whose `decision_kinds()` matches the candidate's `DecisionKind`, calling `activation()` then `verdict()`, **for every candidate action, at every node the AI search expands.** On a large late-game board that is thousands of `verdict()` calls per decision. Treat `verdict()` and everything it transitively calls as an inner-loop function — the same discipline you would apply to code inside the engine's search.

**The trap that shipped (`x_cast_gate`, commit `3de827350d`).** `XCastGatePolicy::verdict()` calls `engine::game::max_x_value` per candidate. `max_x_value` (`casting_costs.rs:7592`) walks the whole battlefield calling `feasible_mana_capacity` per permanent, and for a spell adds a per-X `concrete_cost_for_x` cost-orchestration loop — a board-wide affordability sweep. Run per candidate per search node on a turn-41 board, it regressed the Court of Grace decision from ~2.1s to ~5.7s (isolated A/B). The policy was rules-correct and passed `cargo ai-gate` with 0 win-rate flips; the entire cost was wall-clock.

Rules:

1. **Order predicates cheap-to-expensive; short-circuit on card-local AST before anything board-wide.** Check the candidate's own `ManaCost` / `AbilityCost` / effect chain (a handful of `matches!` over one card's AST) first, and reach for a board-wide or affordability engine call only after the card-local checks prove the candidate is even a member of the class you gate. `x_cast_gate` correctly gates `max_x_value` behind an `{X}`-shard check — but any predicate that ANDs a board-wide call with a card-local one must run the card-local one FIRST. AND is commutative, so the ordering is free latency. (For `x_cast_gate` specifically, the card-local `no_op_at_x_zero` payoff walk should precede the board-wide `max_x_value` — it spares the sweep for every `{X}` card whose payoff has a fixed residual or does not scale with X.)

2. **These engine calls are board-wide or state-cloning — never call them unguarded in `activation()`/`verdict()`:** `max_x_value` / `feasible_mana_capacity` (affordability sweep), `find_legal_targets` (self-heals to a full scan), any `SimulationFilter`-cloning path, and mana-availability sweeps (the pass-priority mana clone storm and declare-attackers mana sweep are documented quadratic hot paths). Prefer a `*_for_simulation` variant where one exists. If you must call one, gate it behind the cheapest structural discriminator and push it as late in the predicate as the logic allows.

3. **`activation()` is the free opt-out; use it.** It receives only `(features, state, player)` — not the candidate — so it cannot inspect the specific card, but it CAN switch the whole policy off for decks/states the policy never fires on (`if features.<feat>.commitment < FLOOR { None }`). A policy that returns `Some(1.0)` unconditionally (an `activation-constant` backstop, like `x_cast_gate`) pays full `verdict()` cost on every matching candidate — acceptable only when `verdict()` itself short-circuits cheaply per rule 1.

4. **`cargo ai-gate` is blind to latency, and `cargo ai-perf-gate` is blind to un-instrumented calls.** `ai-gate` measures win-rate only. `ai-perf-gate` (`crates/phase-ai/src/bin/ai_perf_gate.rs`) field-wise sums engine `PerfCounterSnapshot` fields (`crates/engine/src/game/perf_counters.rs`) against `crates/phase-ai/baselines/perf-baseline.json` — but it only sees paths that bump a counter. `max_x_value` / `feasible_mana_capacity` bump none, so BOTH gates missed the regression above; a manual wall-clock A/B caught it. Therefore, if your policy adds a board-wide/affordability engine call that is not already counter-instrumented, EITHER add a `PerfCounterSnapshot` field for it (and refresh the perf baseline) so `ai-perf-gate` can see it, OR run and attach a wall-clock A/B on a large late-game board showing no material regression. A 0-flip `ai-gate` is necessary but NOT sufficient.

---

## 3. AST type table — where things live

This table is the single biggest research-saver. Every column has been verified against engine source during the 9-feature batch.

| Concept | AST type | File:line |
|---|---|---|
| Card face (the deck-time entity) | `CardFace { name, card_type, mana_cost, abilities, triggers, static_abilities, replacements, keywords, power, toughness, ... }` | `crates/engine/src/types/card.rs:30+` |
| Core type set | `CardFace.card_type.core_types: Vec<CoreType>`; `CoreType::{Creature, Instant, Sorcery, Land, Artifact, Enchantment, Planeswalker, Battle, Tribal, Kindred}` | `card_type.rs:74-77` |
| Mana cost / value | `CardFace.mana_cost: ManaCost`; `ManaCost::mana_value() -> u32` | `card.rs:51`; `mana.rs:468` |
| Face-level keywords | `CardFace.keywords: Vec<Keyword>` (including `Keyword::Prowess`, `Keyword::Haste`, `Keyword::EtbCounter { counter_type, count }`, etc.) | `card.rs:60`; `keywords.rs:24+` |
| Ability definition | `CardFace.abilities: Vec<AbilityDefinition>`; `AbilityDefinition.kind: AbilityKind::{Spell, Activated, Triggered, Static}` | `card.rs:62`; `ability.rs:3749` |
| Ability cost | `AbilityDefinition.cost: Option<AbilityCost>`. **Use `ability.cost_categories()`** (returns `Vec<CostCategory>`) — never destructure variants. | `ability.rs:1748+` |
| Effect chain | `AbilityDefinition.effect: Effect` + `sub_ability: Option<Box<AbilityDefinition>>`. Use `crate::ability_chain::collect_chain_effects(ability)` to walk. | `ability.rs:2080+` |
| Triggered ability | `CardFace.triggers: Vec<TriggerDefinition>`; key fields: `mode: TriggerMode`, `valid_card: Option<TargetFilter>`, `valid_target: Option<TargetFilter>`, `counter_filter: Option<CounterTriggerFilter>`, `constraint: Option<TriggerConstraint>`, `execute: Option<Box<AbilityDefinition>>` | `card.rs:61`; `ability.rs:4515-4600` |
| Trigger modes | `TriggerMode::{ChangesZone, SpellCast, SpellCastOrCopy, Attacks, AttackersDeclared, YouAttack, TokenCreated, TokenCreatedOnce, CounterAdded, CounterAddedOnce, CounterAddedAll, CounterTypeAddedAll, ...}` | `triggers.rs:24+` |
| Trigger constraints | `TriggerConstraint::{NthSpellThisTurn { n, filter }, MaxTimesPerTurn { count }, ...}` | `ability.rs:4484+` |
| Static ability | `CardFace.static_abilities: Vec<StaticDefinition>`. Note: on `GameObject` the field is `static_definitions` (different name!). | `card.rs:63`; `ability.rs:4677-4694` |
| Continuous mods (anthems) | `StaticDefinition.modifications: Vec<ContinuousModification>`; key variants: `AddPower { value }`, `AddToughness { value }`, `AddKeyword { keyword }`, `PowerToughnessAdd`, `GrantTrigger`, `AddStaticMode`, `AddAllCreatureTypes`, ... | `ability.rs:5005-5100` |
| Static mode | `StaticDefinition.mode: StaticMode::{Continuous, AdditionalLandDrop { count }, MayPlayAdditionalLand, ReduceCost { spell_filter, ... }, ...}` | `statics.rs:115+` |
| Replacement effect | `CardFace.replacements: Vec<ReplacementDefinition>`; `event: ReplacementEvent::{ETB, AddCounter, ...}`; `quantity_modification: Option<QuantityModification::{Double, Plus { value }, Minus { value }}>` | `card.rs:64`; `ability.rs:4818-4874`; replacements at `replacements.rs:39+` |
| Target filter | `TargetFilter::{Any, Player, Opponent, Controller, Typed(TypedFilter), Or { filters }, And { filters }, SelfRef, ...}` | `ability.rs:3083+` |
| Typed filter | `TypedFilter { type_filters: Vec<TypeFilter>, controller: Option<ControllerRef>, properties: Vec<FilterProp>, ... }`; controller-scoping via `ControllerRef::{You, Opponent, ...}` | `ability.rs:1011-1019`, `:813-818` |
| Type filter variants | `TypeFilter::{Creature, Instant, Sorcery, Land, ..., Subtype(String), AnyOf(Vec<TypeFilter>)}` | `ability.rs:794+` |
| Filter properties | `FilterProp::{Attacking, Tapped, CountersGE { counter_type, count }, ...}` | `ability.rs:834-859` |
| Counter type | `CounterType::Plus1Plus1` (typed enum, used in filters/triggers); serialized as string `"P1P1"` (used in `Effect::AddCounter`/`PutCounter`/`Token.enter_with_counters`/`Keyword::EtbCounter`). **Both sides must be checked when classifying counter cards.** | `counter.rs:5-52` |
| Counter trigger filter | `CounterTriggerFilter { counter_type: CounterType, threshold: Option<u32> }` (no `Default` impl — construct fields explicitly) | `ability.rs:4501-4513` |
| Game object (runtime) | `GameObject { name, controller, zone, abilities, static_definitions, replacement_definitions, card_types, mana_cost, counters: HashMap<CounterType, u32>, keywords, ... }`. Note: `GameObject` has NO `triggers` field — triggers are registered in the trigger-watcher subsystem. | `game/game_object.rs:25+` |
| Game state | `GameState { battlefield, stack, players, objects, current_starting_player, ... }`. Stack entries via `state.stack.iter()`; resolved abilities via `entry.ability() -> Option<&ResolvedAbility>`. | `game_state.rs:30+` |
| Game action | `GameAction::{CastSpell { object_id, ... }, ActivateAbility { source_id, ability_index }, DeclareAttackers { attacks }, PlayLand { ... }, ...}` | `actions.rs:20+` |
| WaitingFor (decision context) | `WaitingFor::{Priority { player }, DeclareAttackers { valid_attacker_ids, ... }, ChooseTarget { ... }, BetweenGamesSideboard, ...}`. Engine pre-computes `valid_attacker_ids` (CR 508.1a) — use that, don't re-scan. | `game_state.rs:430+` |

**GameObject vs CardFace API drift.** This bites every feature. CardFace has `static_abilities`/`replacements`; GameObject has `static_definitions`/`replacement_definitions`. Detection (CardFace) and policy (GameObject) need parallel access — use parts-based predicates (Section 5).

---

## 4. Required artifacts per feature

Every feature must produce ALL of these:

1. **AST verification table in module docstring** — one line per axis citing the exact `crates/engine/src/types/...:line` where the type lives + a CR number where applicable. Mirror `landfall.rs:1-12` and `aggro_pressure.rs:1-25`. This is your proof that the AST supports detection without parser changes.
2. **`<Name>Feature` struct** — counters as `u32`, ratios as `f32`, identity-lookup lists as `Vec<String>`, single `commitment: f32` (0.0..=1.0). NO bool fields. Document each field with a CR if it implements a rule.
3. **Commitment formula** with calibration anchor in a doc comment. Commitments are density-normalized per 60 nonland cards so 60-card and 99-card decks with the same density score the same. Use only the shared helpers in `features/commitment.rs`: `commitment::density_per_60(count, total_nonland)`, `commitment::weighted_sum(&[(weight, density)])`, and `commitment::geometric_mean(&[pillar_density])`. Show the math AND a real-deck calibration ("Mono-Red Burn: ~16 one-drops + 4 burn → commitment ≈ 0.85"). Anti-calibration ("UW control → commitment ≈ 0.0") is also required. Two formula shapes are established:
   - **Weighted sum, clamped** (when missing pillars are tolerable). Pattern: control, aggro_pressure, spellslinger.
   - **Geometric mean with zero-pillar collapse** (when missing a pillar = not this archetype). Pattern: aristocrats, tokens_wide.
4. **Parts-based predicate exports** — one `pub(crate) fn is_<X>_parts(<minimal CardFace slices>) -> bool` per axis. Each takes the slice the policy actually has access to on `GameObject` (e.g., `is_burn_spell_parts(core_types: &[CoreType], abilities: &[AbilityDefinition])`). Add a public `is_X(face: &CardFace)` wrapper if useful for tests; remove it if dead per clippy. **The `_parts` predicates are the contract** — they let the policy classify live `GameObject`s without reconstructing a `CardFace`.
5. **Threshold constants** — `pub const`. Mirror `tribal.rs:47-67`: `LORD_PRIORITY_FLOOR`, `MULLIGAN_FLOOR`, `AGGRO_TEMPO_FLOOR`. Each has a docstring explaining the semantic level it gates.
6. **CR annotation table** — one CR per rule-bearing comment, every number grep-verified. Don't annotate utility/plumbing code.
7. **Policy specs** — for each tactical/mulligan policy: `id()`, `decision_kinds()`, `activation()` formula, `verdict()` reason categories with stable `kind` strings. Reason kinds are a frozen identifier set.
8. **Wiring sites** — registry/session/plan edits (Section 7).
9. **Test plan** — feature-level (one per axis + calibration anchor + opt-out + clamp), tactical policy (activation gate + each verdict branch), mulligan (each verdict tier).

---

## 5. The parts-based predicate convention

This is the architectural heart. Every classifier exists in two shapes:

```rust
// Public-facing convenience (used in detect() and tests):
pub fn is_burn_spell(face: &CardFace) -> bool {
    is_burn_spell_parts(&face.card_type.core_types, &face.abilities)
}

// Reusable contract (used in policies against GameObject fields):
pub(crate) fn is_burn_spell_parts(
    core_types: &[CoreType],
    abilities: &[AbilityDefinition],
) -> bool {
    abilities.iter().any(|a| {
        a.kind == AbilityKind::Spell
            && collect_chain_effects(a)
                .iter()
                .any(|e| matches!(e, Effect::DealDamage { target, .. }
                                  if filter_can_target_player(target)))
    })
}
```

Why parts-based:
- Detection runs against `CardFace` (the deck list).
- Policies run against `GameObject` (the live battlefield/hand/stack).
- The fields between them have **different names** (`static_abilities` vs `static_definitions`, `replacements` vs `replacement_definitions`) but the same shape.
- Parts predicates take the slice — both call sites pass their respective field. No conversion, no duplication.

**Rule of thumb:** if your detect() uses `face.X`, the corresponding parts predicate takes `X: &[T]` and the policy passes `&obj.X` (or the equivalently-named runtime field).

---

## 6. The TacticalPolicy / MulliganPolicy contract

```rust
// crates/phase-ai/src/policies/registry.rs
pub trait TacticalPolicy: Send + Sync {
    fn id(&self) -> PolicyId;
    fn decision_kinds(&self) -> &'static [DecisionKind];
    fn activation(&self, features: &DeckFeatures, state: &GameState, player: PlayerId) -> Option<f32>;
    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict;
}

// crates/phase-ai/src/policies/mulligan/mod.rs
pub trait MulliganPolicy: Send + Sync {
    fn id(&self) -> PolicyId;
    fn evaluate(
        &self,
        hand: &[ObjectId],
        state: &GameState,
        features: &DeckFeatures,
        plan: &PlanSnapshot,
        turn_order: TurnOrder,
        mulligans_taken: u8,
    ) -> MulliganScore;
}
```

**`activation()` patterns:**
- `if features.<feat>.commitment < FLOOR { None } else { Some(features.<feat>.commitment) }` — opt-out gate + linear scaling. Standard.
- Constant `Some(1.0)` requires an `// activation-constant: <reason>` marker (lint at `policies/tests/activation_marker_lint.rs`).

**`verdict()` patterns:**
- Return `PolicyVerdict::{nudge,preference,strong,critical}(ctx.config.policy_penalties.<named_field>, reason)` for scores. Return `PolicyVerdict::reject(reason)` only for genuine vetoes.
- `PolicyReason::new("stable_kind_string").with_fact("metric_name", value as i64)` — kind is `&'static str`, frozen per policy.
- Mirror `free_outlet_activation.rs:80-94`: re-classify the live ability structurally using parts predicates against `obj.abilities`/`obj.keywords`/etc. — don't trust deck-time classification at decision time.

**`MulliganPolicy::evaluate` patterns:**
- Opt-out below `MULLIGAN_FLOOR` returns `Score { delta: 0.0, reason: <name>_keepables_na }`.
- Walk `hand` (`Vec<ObjectId>`); look up each via `state.objects.get(&oid)`. Classify by core type, mana value, identity lookup against feature `payoff_names`.
- Verdict tiers in priority order (first match returns): `_keepable_ideal` (+1.5..2.0), `_workable` (+0.5..1.0), penalty (-1.0..-1.5), default (0.0).
- `turn_order` and `mulligans_taken` are real inputs. New policies must consume them or carry an `// input-unused: <reason>` marker next to the binding.

---

## 7. Wiring sites (file:line — read each before editing)

For a feature named `<feat>` with `<Feat>Feature` struct and policies `<Feat>Policy` + `<Feat>Mulligan`:

| File | Edit |
|---|---|
| `crates/phase-ai/src/features/mod.rs` | Add `pub mod <feat>;` + `pub use <feat>::<Feat>Feature;` + `pub <feat>: <Feat>Feature` field on `DeckFeatures`. |
| `crates/phase-ai/src/features/<feat>.rs` | NEW. The feature module + tests. |
| `crates/phase-ai/src/features/mod.rs` | Call `<feat>::detect(deck)` in `DeckFeatures::analyze`. |
| `crates/phase-ai/src/policies/mod.rs` | Add `pub mod <feat>;` (and any new tactical policy module). |
| `crates/phase-ai/src/policies/<feat>.rs` | NEW. Tactical policy + tests. |
| `crates/phase-ai/src/policies/registry.rs` | Add `PolicyId::<Feat>` (and `<Feat>Mulligan`) variants. Import `super::<feat>::<Feat>Policy`. Add `Box::new(<Feat>Policy)` to `PolicyRegistry::default`'s vec. |
| `crates/phase-ai/src/policies/mulligan/<feat>_keepables.rs` | NEW (if a mulligan policy is in scope). |
| `crates/phase-ai/src/policies/mulligan/mod.rs` | Add module + `pub use` + `Box::new(<Feat>Mulligan)` to `MulliganRegistry::default`'s vec. |
| `crates/phase-ai/src/config.rs` | Add the policy's scoring scalar(s) to `PolicyPenalties`, **and register each new field** in `ACTIVE_POLICY_PENALTY_FIELDS` or `UNTUNED_POLICY_PENALTY_FIELDS`. Not optional — a test enforces it. See Section 7a. |
| `crates/phase-ai/src/plan/curves.rs` | OPTIONAL: add tempo-class branch in `tempo_class_for` (carefully ordered — see Section 8). Add expected-curve adjustments in `expected_lands_for`/`expected_mana_for`/`expected_threats_for` if archetype shifts the curve. Plan data is consumed by mulligan bottoming and curve expectations; do not add zombie plan fields. |

The `no_name_matching` lint at `features/tests/no_name_matching.rs` automatically scans both `src/features/` and `src/policies/` — no manual exemption needed unless your file names appear in the lint's allow-list (unlikely).

---

## 7a. `PolicyPenalties` registration — ACTIVE vs UNTUNED

Every scoring scalar you add to `PolicyPenalties` **must** be listed in exactly one of two sets in `crates/phase-ai/src/config.rs`. This is enforced, not advisory:

- `every_policy_penalty_is_tuning_registered_or_explicitly_untuned` (`config.rs:1342`) serializes `PolicyPenalties::default()` and asserts **set equality** between its field names and `ACTIVE_POLICY_PENALTY_FIELDS ∪ UNTUNED_POLICY_PENALTY_FIELDS`. A new field in neither set — or a stale name in either set — turns `cargo nextest run -p phase-ai` red (Tilt `test-ai`; CI runs it workspace-wide via the `cargo nextest run --profile ci … --workspace` step in `.github/workflows/ci.yml`). The same test requires every `UNTUNED_` entry's reason string to be non-empty.

**Which set?** `ACTIVE_POLICY_PENALTY_FIELDS` (`config.rs:617`) *is* the CMA-ES parameter vector: `ai_tune` builds the `--group penalties` parameter names from it (`bin/ai_tune.rs:102`) and `policy_penalties_from_params` deserializes the optimizer's output back over exactly those fields, clamped to `[-15.0, 15.0]` (`bin/ai_tune.rs:222`). Anything listed there is a **free variable the optimizer will overwrite**.

- **`ACTIVE_`** — soft heuristics whose magnitude is a preference (nudge/preference/strong band). Only after a paired-seed `cargo ai-gate` calibration; that's what every current `UNTUNED_` reason is waiting for.
- **`UNTUNED_`** (`config.rs:658`, `(field, reason)` pairs) — the default, and the **mandatory** home for any game-deciding scalar whose value is load-bearing for correctness rather than taste (a `critical`-band win-detector weight, e.g. a CR 104.2a "last player standing" crown term). Listing such a scalar in `ACTIVE_` licenses CMA-ES to tune a win detector into noise.

When in doubt, ship in `UNTUNED_` with a reason. Promotion is a one-line move backed by an ai-gate report; demotion after the optimizer has already smeared the value is not.

---

## 8. Plan/curves placement order (critical)

`tempo_class_for` in `plan/curves.rs` returns the FIRST matching `TempoClass`. Order matters because hybrid decks should resolve to a sensible class.

Established order (DON'T break this):
1. **Ramp first** (landfall.commitment > 0.5 OR mana_ramp.commitment > 0.5) → `Ramp`. Ramp+anything reads as Ramp.
2. **Tribal** (tribal.commitment > AGGRO_TEMPO_FLOOR) → `Aggro`. Tribal+aggro reads as Aggro (tribal wins position).
3. **Aggro pressure** (aggro_pressure.commitment >= AGGRO_TEMPO_FLOOR) → `Aggro`.
4. **Tokens wide** (tokens_wide.commitment >= TOKENS_WIDE_TEMPO_FLOOR) → `Aggro`.
5. **Control** (control.commitment > 0.55 AND control.reactive_tempo > 0.35) → `Control`.
6. **Aristocrats** (aristocrats.commitment > 0.5) → `Midrange`.
7. **Plus-one counters** (plus_one_counters.commitment > 0.5) → `Midrange`.
8. **Default to archetype**.

When inserting a new branch: ramp-style features go FIRST (they dominate), aggro/midrange/control go in the middle, "fallback" archetypes last. Use `>=` for consistency; the existing `>` in some branches is being migrated.

---

## 9. Recurring review traps (avoid these)

These are real bugs caught in the 9-feature batch's reviews. Each cost a follow-up commit.

| Trap | Caught in | Fix pattern |
|---|---|---|
| **CR drift** — citing a rule number from memory that's actually a different rule | Counters review caught CR 122.1d (Stun, not +1/+1) and CR 701.27 (Transform, not Proliferate). Aggro caught CR 702.2 (Deathtouch, used for keyword field) and CR 800.4 (multiplayer, used for min-life helper). | Grep `docs/MagicCompRules.txt` for every CR before writing. Don't annotate plumbing. |
| **Per-copy name push** — `payoff_names.push(name)` inside `for _ in 0..entry.count` loop inflates the list to playset size | Tokens review caught (4× pushes per Bitterblossom). | Hoist the push outside the loop. Counts use `entry.count`; identity-lookup lists use one push per unique face. |
| **Combo card double-push** — `if is_X { names.push() } if is_Y { names.push() }` pushes the same name twice when both flags fire | Tokens review caught (Glorious Anthem + Overrun-shape). | Use `if is_X \|\| is_Y { names.push() }` once per face. |
| **Opponent-scoped trigger counted as your payoff** — failing to reject `valid_target = ControllerRef::Opponent` makes "punisher" cards register as your payoffs | Counters review caught. | Mirror `aristocrats::typed_filter_is_creature_you_control`'s `if matches!(typed.controller, Some(ControllerRef::Opponent)) { return false; }`. |
| **Stack inspection only walks head effect** — `state.stack.iter().for_each(\|e\| check(&resolved.effect))` misses counter effects in `sub_ability` | Counters review caught. | Use `super::context::collect_ability_effects(resolved)` to walk the full ResolvedAbility chain. |
| **Doubler check on activated source** — passive replacements (Hardened Scales, Doubling Season) live on permanents, not on activated abilities | Counters review caught the dead branch. | Scan `state.battlefield` for permanents with `replacement_definitions` matching the predicate. |
| **`is_X` wrapper unused** — clippy flags wrappers without callers | Spellslinger reviewer noted; agent removed in implementation. | Keep `_parts` predicates always; add public wrappers only if a real caller exists, otherwise remove. |
| **Loose calibration assertion** — `assert!(commitment > FLOOR)` doesn't enforce the docstring's calibration anchor | Spellslinger review caught (burn floor was 0.30 vs doc claim 0.40). | Tighten test bound to match the calibration claim. |
| **Misnamed test** — `opponent_scope_payoff_ignored` actually tested the no-counter-filter case | Counters review caught. | Verify test names match what they assert. |
| **`git add -A` picks up untracked planning files** at repo root | Aggro fix-up commit accidentally included `parser-batch-plan.md`/`parser-fallback-plan.md`. | ALWAYS use explicit `git add path1 path2 ...`. The repo root has untracked files unrelated to AI work. |

---

## 10. Cross-feature interaction patterns

When your feature should influence an existing policy, prefer **surgical amplification** over a new policy:

```rust
// Inside an existing policy's verdict() or score():
let amp = 1.0 + (features.<your_feat>.commitment as f64).clamp(0.0, 1.0) * 0.5;
penalty *= amp;
```

Examples shipped:
- `tokens_wide` amplifies `BoardWipeTelegraphPolicy` (wide-board decks fear sweepers more).
- `mana_ramp::is_mana_dork_parts` is consumed by `HoldManaUpForInteractionPolicy` (mana sources count untapped Sol Ring shapes).
- `landfall::ability_searches_library_for_land` is consumed by `aristocrats` (anti-fetchland gate prevents fetchlands counting as outlets).

When a helper needs to be shared across two features, **promote it to `pub(crate)`** in the owning module with a doc note ("Shared with X for Y"), don't recreate it.

---

## 11. Test plan template

Every feature MUST have:

**Feature tests:**
- `empty_deck_produces_defaults`
- `vanilla_creature_not_registered`
- `detects_<each_axis>` (one per detection axis)
- `<exclusion>_does_not_count` (negative cases — e.g., `treasure_token_does_not_count`, `reach_does_not_count_as_evasion`, `loyalty_counter_not_counted`)
- `opponent_scope_<X>_ignored` (controller rejection where applicable)
- `<calibration_anchor_name>_hits_floor` (positive calibration: `assert!(commitment > 0.85)`)
- `<negative_archetype>_below_floor` (negative calibration)
- `commitment_clamps_to_one` (overflow saturation)
- `<identity_list>_dedup` (one push per unique face — see Section 9 trap)

**Tactical policy tests:**
- `activation_opts_out_below_floor`
- `activation_opts_in_above_floor`
- One test per `verdict` branch (positive scoring case)
- One test per `verdict` branch (negative/penalty case)
- `non_<X>_action_returns_zero` (`<X>_na` reason kind)

**Mulligan policy tests:**
- `opts_out_when_commitment_low`
- `ideal_hand_<descriptors>` → positive delta with `<feat>_keepable_ideal` reason
- Each verdict tier (workable, penalty, default)
- A `turn_order` / `mulligans_taken` test, or an explicit `// input-unused: <reason>` marker in the implementation.

**Measurement evidence:**
- New-policy PRs attach a `rtk cargo ai-gate` paired-seed report. No flips is acceptable for narrow policies, but the run must exist.
- If the policy calls any board-wide or affordability engine function (`max_x_value`, `find_legal_targets`, mana-availability / `SimulationFilter` sweeps), ALSO attach `cargo ai-perf-gate` (or a wall-clock A/B on a large late-game board). `ai-gate` measures win-rate only and is structurally blind to per-decision latency; `ai-perf-gate` is blind to any call that bumps no `PerfCounterSnapshot` field. See the **Performance** section.

---

## 12. Implementation order (each step compiles before the next)

1. Read `landfall.rs` + the most-similar reference feature end to end.
2. Verify each CR number you'll cite by grepping `docs/MagicCompRules.txt`.
3. STEP 0 (if needed): refactor an existing predicate from another feature to `pub(crate)` parts shape (e.g., `control::is_card_draw_parts` was promoted for spellslinger). Run the existing tests; behavior must be preserved.
4. Optionally start from `scripts/new-ai-policy.sh <feat>`; it writes the three scaffold files and prints the wiring checklist.
5. Create `features/<feat>.rs`: struct + thresholds + `detect()` + parts predicates + tests.
6. Wire `features/mod.rs`.
7. Create `policies/<feat>.rs` (tactical) + tests. Add `PolicyId` variant + `Box::new()` registration.
8. Create `policies/mulligan/<feat>_keepables.rs` + tests. Add `PolicyId` variant + register in `MulliganRegistry::default`.
9. (Optional) Add `plan/curves.rs` branches with tests.
10. (Optional) Cross-feature amplification (Section 10).
11. Run `cargo fmt --all`, then use Tilt verification: `./scripts/tilt-wait.sh clippy test-ai`. If Tilt is down, fall back to the direct phase-ai clippy/test commands.
12. Run `rtk cargo ai-gate` for new policy work.
13. Commit with explicit `git add path1 path2 ...`. NEVER `git add -A`.
14. Spawn an opus advisory review (see `git log --grep="address review"` for prior fix-up commit messages as templates). Address MUST FIX in a follow-up commit; CONSIDER items at your discretion.

---

## 13. Sample input contract (when invoking this skill)

When calling this skill or asking an agent to implement a new feature, provide:

1. **Feature name** (`<feat>`, snake_case).
2. **Detection axes** — one bullet per axis with the AST type to inspect (use the table in Section 3).
3. **Calibration anchor** — a real archetype's component counts so the formula can be sanity-checked.
4. **Policies in scope** — tactical only, or also mulligan, and any cross-feature amplification.
5. **Boundary discussion** — overlap with existing features (e.g., "tokens_wide overlaps with aristocrats on token generators; both axes are independent — a deck can score high in both").

The skill provides everything else (struct shape, formula choice, predicate signatures, CR table, wiring, tests, traps).
