# Plan: Confine `XCastGatePolicy`'s board-wide affordability sweep to the root decision

## 0. Problem restatement & confirmed regression mechanism (re-verified against live source)

`XCastGatePolicy::verdict` (`crates/phase-ai/src/policies/x_cast_gate.rs:68`) → `gate_rejects`
(`:187`) calls `engine::game::max_x_value` (`crates/engine/src/game/casting_costs.rs:7592`) for
every `{X}`-cost `CastSpell`/`ActivateAbility` candidate. `max_x_value` (verified at source) sweeps
the **whole battlefield** (`state.battlefield.iter().map(|id| feasible_mana_capacity(...))` at
`casting_costs.rs:~7660`) plus pool + delve accounting. That is a board-wide affordability sweep.

**Invocation volume (verified by tracing, not assumed):**

- `PolicyRegistry::verdicts`/`score` (`registry.rs:375,439`) runs XCastGate whenever the candidate
  classifies as `CastSpell`/`ActivateAbility`.
- The only production path that reaches `verdicts`/`score` for these decision kinds is
  `PlannerServices::tactical_score` (`planner/mod.rs:908`), which builds `PolicyContext` at `:923`
  and calls `self.policies.score(&policy_ctx)` at `:932`.
- `tactical_score` is called at **three production sites**:
  1. `search.rs:1786` — inside `score_candidates_core` (`search.rs:1605`), the **root** ranking
     prior (`services.tactical_score(state, &ctx, &g.candidate, ai_player)` over root `gated`
     candidates). `score_candidates_core` is the single top-level decision function (its only
     production callers are `search.rs:1562` and `:1595`); it is **not** recursive.
  2. `search.rs:1886` — the heuristic-only branch of `score_candidates_core` (also **root**).
  3. `planner/mod.rs:1108` — inside `BeamContinuationPlanner::search_value` (`planner/mod.rs:1064`),
     the alpha-beta move-ordering scorer, called at **every interior beam node**. `search_value`
     recurses at `:1130`, and is entered from `evaluate_after_action` (`:1163`) which
     `score_candidates_core` calls at `search.rs:1852` **after** `apply_candidate` — i.e. every
     `search_value` frame is already ≥1 ply below the decision root (never root).
- Rollout leaves add a fourth path: `search_value` at `depth==0` calls `rollout_estimate`
  (`planner/mod.rs:1076`) → `planner_evaluation` (`:971`) → `policy_priors` (`:961`) →
  `PolicyRegistry::priors` (`registry.rs:457`), which builds `PolicyContext` at `:474` and calls
  `score`. Rollout is the deepest lookahead. (`policy_priors`'s only other callers are tests.)

So each `{X}` object is scored **once per node** but appears across **thousands of interior/rollout
nodes**, each cloning a different `GameState` — the sweep runs fresh every time because AI search is
simulation-mode and the display-derive board-global mana cache (`derived.rs`) is skipped.

**`gate_candidates`/`assess_pre_cast` (`tactical_gate.rs:99,183`) does NOT run the registry** — it is
a separate hand-rolled pre-cast filter (SourceMatchesFilter, lethal-window, counter-empty-stack,
redundant-removal, pump-window). XCastGate's `Reject` therefore reaches the committed decision
**exclusively** through `policies.score` inside `tactical_score` at the root (`search.rs:1786/1886`):
a `Reject` yields `f64::NEG_INFINITY` (`registry.rs:444`), which makes `RankedCandidate.score`
`NEG_INFINITY` (`search.rs:1789`), so the candidate sorts to the bottom / is truncated
(`search.rs:1802`) and every deepening rung keeps it at `NEG_INFINITY`
(`cont + r.score*tactical_weight`, `search.rs:1853`) — never committed. This is exactly the reported
bug fix ("AI cast Exsanguinate for 0 on turn 1"), and it lives at the **root**.

**Conclusion:** the gate's correctness purpose is fully served at the root. Every interior/rollout
firing is near-redundant (an X=0 cast is already penalized by the resulting state eval) and is the
entire wall-clock regression. Fix = **run `max_x_value` only at the root; return neutral in
lookahead**, plus fold in the safe card-local reorder as a cheap pre-filter at root.

---

## Approaches re-confirmed rejected (from the task brief, verified against source)

- **Per-node object memo — rejected.** Each object is scored once per node; repetition is across
  nodes with differing states. A per-node cache collapses nothing and cross-node value reuse is
  unsafe (affordability is state-dependent). Confirmed: `search_value` clones a fresh `sim` per
  candidate (`apply_candidate`, `planner/mod.rs:1208`).
- **Reuse cached board-global mana availability — unavailable.** It lives in the display-derive
  path; simulation-mode search (`apply_as_current_for_simulation`) skips it. Reviving it in sim mode
  is a separate, larger engine effort — out of scope.
- **Pure reorder (card-local payoff walk before the sweep) — partial only.** For X-scaled
  mana-sinks (Exsanguinate/Helix — the hot case) the payoff *is* a no-op at X=0, so
  `no_op_at_x_zero` returns `true` and the sweep still runs. Safe and free, so **folded in** as a
  cheap pre-filter, but not the fix on its own.

---

## Recommended direction (chosen)

Thread a typed **`SearchDepth`** signal into `PolicyContext` (a general, reusable capability mirroring
the existing `deadline_expired()`/`can_afford_projection()` budget-gating precedent), and have
`XCastGatePolicy` run the board-wide `max_x_value` sweep only when `ctx.at_root()`, returning neutral
otherwise. **Root-only** (not shallow ≤ N) is the principled threshold — justification in §Rust
Idioms / Logic Placement below.

**Engine primitive (`can_afford_x_at_least_one`) — explicitly considered and DEFERRED.** The gate
only needs the boolean `max_x_value == 0`, and a short-circuit boolean could stop summing permanent
capacity once it crosses `fixed_portion + x_count`. But: (a) the depth gate already reduces the call
volume from thousands (interior+rollout nodes) to one-per-candidate-at-root — orders of magnitude —
so the marginal per-call saving at root is small; (b) it still must sweep permanents until the
threshold is crossed (no asymptotic win on the dominant `feasible_mana_capacity` loop); (c) it is an
independent engine micro-optimisation orthogonal to the regression, and the AI/policy floor is
escalated — minimal, clearly-justified engine changes only. **We change zero engine files.** If a
root-only wall-clock A/B still shows `max_x_value` as a material hotspot at the committed decision,
adding `can_afford_x_at_least_one` becomes a separate, independently-reviewable follow-up. This
keeps scope inside `phase-ai`.

---

## Applicable skills (Step 1)

- **`add-ai-feature-policy`** — authoritative for `phase-ai` policy/`PolicyContext` work. Its new
  **Performance** section codifies exactly this regression class (it names `x_cast_gate` /
  `3de827350d` as *the trap that shipped*) and the measurement rule: `ai-gate` is blind to latency,
  `ai-perf-gate` is blind to un-instrumented calls (`max_x_value` bumps no `PerfCounterSnapshot`
  field), so a wall-clock A/B on a large late-game board is mandatory when a board-wide/affordability
  call is added or moved. Rule #1 (order predicates cheap→expensive; card-local before board-wide) is
  the reorder we fold in.
- **`add-engine-variant`** — consulted for the new `SearchDepth` enum. This is a **new AI-crate enum
  in `phase-ai`**, not one of the gated engine AST enums (`QuantityRef`/`Effect`/`FilterProp`/…). The
  parameterization/categorical-boundary/inventory checks target engine `types/*`. `SearchDepth`
  models an AI-search position, has no CR section, is not serialized, and is not in
  `data/engine-inventory.json`'s surface. The gate still applies: §Variant Discoverability below runs
  the checklist and shows there is no existing sibling to parameterize into.
- **Not applicable:** `add-engine-effect`, `oracle-parser`, `add-keyword/trigger/static/replacement`,
  `casting-stack-conditions`, `add-frontend-component`, `add-card-data-pipeline`, `card-test`
  (no cast-pipeline `GameScenario` test is added — all tests are policy-unit + search-level; see
  Verification Matrix). No parser files change → Nom Compliance is N/A (stated explicitly per gate).

---

## Analogous feature traced end-to-end (Step 2 — hard gate)

**Traced feature: the `deadline` / `projection-budget` gating capability on `PolicyContext`.** This
is the exact precedent the task brief names, and it is the same shape (a per-decision signal on the
policy context that expensive policies read to gate their own work). Full trace path followed in
source:

1. **Signal origin (engine):** `engine::util::Deadline` — captured per decision.
2. **Storage:** `AiContext.deadline` (`crates/phase-ai/src/context.rs`), built once per decision in
   `build_ai_context_with_session` (`search.rs:1903`) and held by `PlannerServices.context`.
3. **Context accessor:** `PolicyContext::deadline_expired` (`context.rs:42`) and
   `PolicyContext::can_afford_projection` (`context.rs:52`) — read `self.context.deadline`, return a
   bool, documented as "policies doing non-essential expensive work should short-circuit via this".
4. **Consumer:** velocity/projection policies call `ctx.can_afford_projection()` before
   `get_or_project`; tests `deadline_expired_gates_projection` / `fresh_deadline_allows_projection`
   / `zero_projection_floor_always_allows` (`context.rs:490,542,590`) prove both branches.

**Key structural difference (and why it dictates the design):** `deadline` is a **per-decision**
value → it lives in `AiContext` (shared, one immutable borrow for the whole search). `SearchDepth` is
a **per-node** value that *varies within a single decision's tree* → it cannot live in `AiContext`;
it must be a field on `PolicyContext` itself, set at each construction site (exactly like the
existing per-node `cast_facts` field). This is the one deviation from the traced precedent, and it is
forced by the binding time. Everything else mirrors it: typed value, documented accessor method,
policy reads it to gate expensive work.

Trace path recorded: `context.rs` (`AiContext.deadline` → `PolicyContext::deadline_expired`) →
consumers → `context.rs` tests. Our change adds a sibling capability
(`PolicyContext.search_depth` → `PolicyContext::at_root`) following the same accessor+test shape.

---

## Files read before planning (Step 3)

`x_cast_gate.rs` (full), `context.rs` (full, incl. tests), `registry.rs:360-489`, `planner/mod.rs`
(`tactical_score` 908-952, `policy_priors`/`planner_evaluation`/`rollout_estimate` 954-1046,
`BeamContinuationPlanner`/`search_value`/`evaluate_after_action` 1058-1182, `rank_candidates`
1184-1206), `search.rs` (`emit_trace_for_candidate` 585-607, `score_candidates_core` deepening
loop 1760-1896, callers 1562/1595), `tactical_gate.rs:99-340` (`gate_candidates`/`assess_pre_cast`),
`casting_costs.rs:7560-7700` (`max_x_value`), `add-ai-feature-policy` SKILL Performance section.

---

## Architectural sections

### Pattern Coverage

The regression class is **"an expensive board-wide/affordability engine call inside a `verdict()`
that only needs to be correct at the committed decision."** The new `SearchDepth` capability is a
**general primitive** any such policy can adopt: it is not `x_cast_gate`-specific. Candidates that
could reuse it today already exist in the codebase (board-wide/affordability calls flagged by the
skill's Performance §2: `find_legal_targets`, mana-availability sweeps, `SimulationFilter` clones).
Immediate consumer count: 1 (`XCastGatePolicy`). Reusable-for count: the whole class of veto/commit
policies whose only job is to stop a *committed* action — order ~dozens of registry policies, of
which the board-wide ones are the intended future adopters. This satisfies "build the building
block, not the special case": we ship a `PolicyContext` capability, not a private flag on the gate.

### Building Blocks (compose from existing; justify anything new)

- **Reuse `PolicyContext` accessor precedent** (`deadline_expired`, `can_afford_projection`,
  `context.rs:42/52`) — add `at_root()` in the same style.
- **Reuse the existing `gate_rejects` card-local machinery unchanged** — `spell_x_manacost`,
  `activated_x_manacost`, `mana_cost_has_x` (`x_cast_gate.rs:78-99`), `payoff_effects_and_x_ref`
  (`:111`), `no_op_at_x_zero` (`:156`), `x_reference::*`, `self_cost::effect_is_trivial`. We only
  **reorder** them relative to `max_x_value` and prepend the depth guard; no helper logic changes.
- **Reuse `PolicyVerdict::neutral`/`reject` + `PolicyReason`** (already used at `x_cast_gate.rs:70`).
- **New symbol (justified):** `SearchDepth` enum + `PolicyContext.search_depth` field +
  `PolicyContext::at_root`. New because no existing type expresses "node position within the current
  decision's search tree"; the existing per-node field (`cast_facts`) is a payload, not a position,
  and `AiContext.deadline` is per-decision (wrong binding time — see trace). §Variant
  Discoverability proves there is no sibling to parameterize.

### Logic Placement

- **`SearchDepth` type + `PolicyContext.search_depth` field + `at_root()` accessor →
  `policies/context.rs`.** It is part of the policy-context contract that policies consume; it lives
  beside the `deadline_expired`/`can_afford_projection` accessors it mirrors.
- **Depth *values* are set at the search/planner layer** (`search.rs`, `planner/mod.rs`,
  `tactical_gate.rs`, `registry.rs`) — the only code that knows a node's position. Policies never
  compute depth; they only read `at_root()`. This keeps the search as the single authority for the
  signal and the policy as a pure consumer.
- **The gate decision stays entirely in `x_cast_gate.rs::gate_rejects`.** No engine change.

**Root-only vs shallow (≤ N) — decision + justification.** Root-only. From the traced search
structure: (1) the gate's *sole correctness role* is stopping the **committed** action, which is
decided only at `score_candidates_core` (the root); interior/rollout firings never change the
committed action because the root reject already yields `NEG_INFINITY` for that candidate. (2) At
interior nodes an X=0 cast is naturally dominated by its own resulting state eval (one fewer card, no
payoff), so the beam does not propagate it as best even without the gate. (3) Firing at shallow
depths (1..N) would re-introduce the exact board-wide sweep we are removing, on the very nodes that
are most numerous per decision — the regression's bulk lives at depth ≥ 1. The only thing lost at
depth ≥ 1 is *move-ordering* nicety (an X=0 cast might be tried in the beam), which is a search-node
budget cost, not a correctness cost, and is dominated by the sweep cost we save. Therefore the payoff
of shallow firing is strictly negative. `ai-gate` (paired-seed) is the empirical check: if it shows a
material W→L regression attributable to move-ordering, the documented fallback is `Lookahead` →
depth ≤ 1 by extending the enum payload (see Rust Idioms) — but the plan commits to **root-only** and
requires the `ai-gate` evidence to justify any change.

### Rust Idioms

- **Typed enum, not a bool (no-bool-flags rule).** `SearchDepth { Root, Lookahead }` — a two-variant
  payloadless enum. `Root` = the node the AI commits at; `Lookahead` = any hypothetical
  beam/rollout node. The field is `search_depth: SearchDepth`, never a `is_root: bool`.
- **Why payloadless (JIT / don't pre-compute unused values).** The only consumer is `at_root()`.
  Threading an exact ply count through `search_value`/`rollout` (and inventing a rollout ply number)
  would be speculative machinery no caller reads — the user's KISS/JIT rules forbid pre-computing
  values that might not be used. If graded shallow-depth is later needed (only if `ai-gate` demands
  it), the extension is additive: change `Lookahead` → `Lookahead { plies: u32 }` and update the
  three search construction sites, with `at_root()` unchanged. This is the minimal design that fully
  solves the current task and leaves a clean extension seam.
- **Accessor returns bool, field is typed.** `pub fn at_root(&self) -> bool { matches!(self.search_depth, SearchDepth::Root) }` — the no-bool rule governs *fields/params carrying state*, not query-method return types (mirrors `deadline_expired(&self) -> bool`).
- **Exhaustive `match` in `at_root` via `matches!`** — no wildcard; adding a future variant forces a compile-time review of the accessor.
- **`#[derive(Debug, Clone, Copy, PartialEq, Eq)]`** on `SearchDepth` (it is a trivial Copy tag, matching `BeamContinuationPlanner`'s `#[derive(Debug, Clone, Copy)]`).

### Extension vs Creation

**Extension.** We extend the existing `PolicyContext` self-gating pattern (deadline/projection
budget) with a sibling capability, add one field alongside the existing per-node `cast_facts` field,
and only reorder + prepend a guard inside the existing `gate_rejects`. No new pattern, no new module,
no engine surface. The one genuinely new artifact is the `SearchDepth` enum, justified above.

### Variant Discoverability (`add-engine-variant` checklist for `SearchDepth`)

- **Existence check:** `SearchDepth`/`search_depth` do not exist in `phase-ai` (grepped). Not an
  engine AST enum → absent from `data/engine-inventory.json` by design (that file catalogs engine
  `types/*` surface, not AI-search plumbing).
- **Parameterization filter:** Is `SearchDepth` a leaf parameterization of an existing enum's axis?
  No existing enum encodes "search-tree position." `BeamContinuationPlanner.depth: u32` is
  *remaining* alpha-beta depth (a countdown inside one recursion), not distance-from-root and not a
  policy-facing type; overloading it would leak a search-internal counter into the policy contract
  and conflate two abstraction layers (the exact "separate abstraction layers" smell CLAUDE.md
  warns against). So a new, purpose-built type is correct, not a sibling-cluster smell.
- **Categorical boundary:** N/A — no CR section; this is heuristic AI plumbing, not a rule enum.
- **Sibling-cluster smell:** none — there is no existing `Root`/`Interior`/`Leaf` cluster to unify.

### Verification Matrix

Legend per row: **seam** (changed function) · **entry** (production caller) · **test**
(add/update) · **revert-failing assertion** · **hostile/sibling/negative** · **first production
branch the fixture reaches**.

**Claim A — Lookahead skips the sweep and returns neutral (the fix).**
- Seam: `gate_rejects` (`x_cast_gate.rs:187`), new first line `if !ctx.at_root() { return None; }`.
- Entry: `tactical_score` @ `planner/mod.rs:1108` (interior) and `registry.rs:474` (rollout).
- Test (NEW, unit): `helix_pinnacle_lookahead_returns_neutral` — Helix fixture, **zero mana**
  (max_x would be 0 → Root rejects), `search_depth: SearchDepth::Lookahead` → `assert_not_reject`.
- Revert-failing assertion: with the depth guard reverted, the Lookahead ctx would reject (max_x=0
  no-op) → `assert_not_reject` fails. This pins the new behavior, not a tautology.
- Paired positive reach-guard (non-vacuous): `helix_pinnacle_max_x_zero_rejected` (existing,
  `:614`) with `search_depth: Root` still rejects. The negative (Lookahead-neutral) is meaningful
  only because the identical fixture at Root rejects — so neutral is caused by depth, not by a
  short-circuit (no `{X}` shard, wrong action, etc.).
- Hostile: assert the Lookahead ctx is *reached past* the `{X}`-shard check — i.e. use a genuine
  `{X}` no-op fixture so a non-`{X}` early-return can't vacuously satisfy `assert_not_reject`. The
  reach-guard row above (same fixture rejects at Root) proves reachability.

**Claim B — Root still rejects the X=0 no-op commit (bug fix preserved).**
- Seam: `gate_rejects` at `SearchDepth::Root`.
- Entry: `tactical_score` @ `search.rs:1786/1886` (root ranking).
- Test (UPDATE): every existing `x_cast_gate.rs` reject test (`helix_pinnacle_max_x_zero_rejected`,
  `exsanguinate_max_x_zero_rejected_{healthy,life_critical,stale_last_effect_amount}`,
  `mirror_entity_max_x_zero_rejected`, `day_of_black_sun_max_x_zero_rejected`,
  `etb_x_counter_creature_max_x_zero_rejected_via_object_level_x`, `x_referencing_rider_now_gated`)
  — their `PolicyContext` literals (helpers `verdict_for` `:576`, `verdict_for_cast_with_facts`
  `:540`) add `search_depth: SearchDepth::Root`. Assertions unchanged (`assert_reject`).
- Revert-failing assertion: these `assert_reject`s fail if the Root branch is broken.
- Sibling/negative: `helix_pinnacle_max_x_one_not_rejected` (`:626`), `exsanguinate_max_x_one_...`
  (`:684`), `mirror_entity_max_x_one_...` (`:787`), `etb_..._max_x_one_...` (`:904`),
  `interleaved_fixed_gain_then_prev_amount_not_gated` (`:700`), `fixed_generic_keyword_grant_...`
  (`:732`), `non_x_cost_activation_not_gated` (`:814`), `multi_spell_ability_object_not_gated`
  (`:834`) — all with `search_depth: Root`, all still `assert_not_reject` (proves Root neutrality
  paths are preserved and the reorder didn't over-gate).

**Claim C — top-level committed decision still refuses the X=0 cast (end-to-end, discriminating).**
- Seam: `score_candidates_core` (`search.rs:1605`) via `tactical_score`@`:1786`.
- Entry: `score_candidates_core` (production top-level scorer; existing test harness pattern at
  `search.rs:2915` `via_core = score_candidates_core(&state, PlayerId(0), &config, &session, None)`).
- Test (NEW, search-level): `xcast_zero_no_op_not_committed_at_root` — build a `GameState` where the
  AI's only `{X}` action is a max-X=0 no-op (e.g. Helix-shape activated ability on battlefield with
  zero available mana, plus a benign `Pass`), run `score_candidates_core`, assert the returned
  scored list gives the `ActivateAbility` a non-finite / minimal score (i.e. it is not the argmax;
  `Pass` outranks it). This proves the fix survives at the real decision seam, not just in the unit.
- Revert-failing assertion: if the Root gate were also disabled, the X=0 action would score finite
  and could win → the "not the argmax / non-finite" assertion fails.
- Reach-guard (non-vacuous, paired): a sibling assertion with **enough mana for X≥1** → the same
  action scores finite and is a legitimate candidate (the gate stands down), proving the refusal is
  affordability-driven, not a blanket suppression.

**Claim D — reorder spares the sweep for fixed-residual `{X}` cards at root (folded-in cheap win).**
- Seam: `gate_rejects` order — `no_op_at_x_zero` (card-local) now precedes `max_x_value`.
- Entry: root `tactical_score`.
- Test (existing, semantics unchanged): `fixed_generic_keyword_grant_not_gated` (`:732`) and
  `interleaved_fixed_gain_then_prev_amount_not_gated` (`:700`) already exercise "payoff not a no-op
  at X=0 → not gated." After the reorder they must still `assert_not_reject`. Their value now
  doubles as proof the early `no_op_at_x_zero` return path works. (No new perf-only unit test — the
  reorder's benefit is measured by the wall-clock A/B, not asserted in a unit test.)
- Note on honesty: the reorder is a pure predicate-ordering change; it cannot change any verdict
  outcome (AND is commutative), so no behavioral test regresses. Stated explicitly.

**Coverage-status impact:** none. No parser change, no Oracle text accepted, no
`Effect::unimplemented` involved — `client/public/coverage-summary.json` is untouched. (Stated per
the parser-honesty requirement: N/A here.)

**Serialized-surface impact:** none. `PolicyContext` is a transient borrow struct (holds `&`
references, `Copy` scalars, and `Option<CastFacts>`); it is never serialized. `SearchDepth` is a
`Copy` tag, not persisted, not in any wire/JSON schema, not in `engine_wasm.d.ts`.

### Identity / Provenance Contract

The gate's existing X-scaling detection (`no_op_at_x_zero`, `x_reference::*`, chain-relative
`PreviousEffectAmount`/`CostXPaid` handling) is **unchanged** — no new "this way"/"chosen"/latched
semantics are introduced. The one new binding is the depth signal:

- **Source concept:** position of the scored node within the current AI decision's search tree
  ("the committed decision" vs "a hypothetical lookahead node").
- **Selected authority + value:** `PolicyContext.search_depth: SearchDepth` (`Root` | `Lookahead`).
- **Binding time:** at each `PolicyContext` construction (per node), set by the search/planner code
  that knows the node's position — never inferred by the policy.
- **Live vs snapshot:** it is a per-construction snapshot of a static fact about *that* node; it does
  not mutate after construction and is not re-read from live state (no invalidation needed — the
  `PolicyContext` is dropped at the end of the candidate's scoring).
- **Storage:** field on the transient `PolicyContext`.
- **Consuming function:** `PolicyContext::at_root` → `x_cast_gate::gate_rejects`.
- **Multi-authority hostile fixture (proves the binding is real, not vacuous):** Claim A's
  Lookahead-neutral test paired with Claim B's Root-reject test use the **identical fixture** (same
  card, same zero-mana state) and differ **only** in `search_depth`. Same inputs → opposite verdicts
  ⇒ the binding is the sole cause. This is the discriminating multi-authority case for the new field.

---

## Step-by-step implementation

All paths under `crates/phase-ai/src/`. No engine files. No CR annotations (heuristic AI plumbing —
the only CR-bearing code, `no_op_at_x_zero`/`max_x_value` call sites, is unchanged; existing CR
comments in `x_cast_gate.rs`/`casting_costs.rs` remain accurate).

### 1. `policies/context.rs` — add the `SearchDepth` primitive, field, and accessor

- Add the type (place near the top, after imports, before `PolicyContext`):
  ```rust
  /// Position of the node being scored within the current AI decision's search
  /// tree. `Root` is the node the AI will actually commit an action at
  /// (`score_candidates_core`); `Lookahead` is any hypothetical node inside beam
  /// alpha-beta or rollout. Expensive policies (board-wide affordability sweeps,
  /// `find_legal_targets`, `SimulationFilter` clones) should run their full
  /// analysis only at `Root` via [`PolicyContext::at_root`] and return neutral in
  /// lookahead, where the resulting-state eval already accounts for the action.
  /// Mirrors the `deadline`/projection-budget self-gating precedent, but is a
  /// per-node field (not an `AiContext` value) because depth varies per node.
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum SearchDepth {
      Root,
      Lookahead,
  }
  ```
- Add field to `PolicyContext` (`context.rs:19-27`), after `cast_facts`:
  `pub search_depth: SearchDepth,`
- Add accessor in `impl<'a> PolicyContext<'a>` (beside `deadline_expired`/`can_afford_projection`):
  ```rust
  /// True when this is the node the AI will commit an action at. Policies whose
  /// only correctness role is stopping a *committed* action (and whose analysis
  /// is board-wide/expensive) should gate that work behind this and return
  /// neutral otherwise — the lookahead eval already dominates no-op lines.
  pub fn at_root(&self) -> bool {
      matches!(self.search_depth, SearchDepth::Root)
  }
  ```
- Update the **test** `PolicyContext` literals in `context.rs` tests (`:252, :314, :382, :455, :478`)
  and the `deadline_test_ctx` helper (`:471-487`) to add `search_depth: SearchDepth::Root,` (bring
  `SearchDepth` into the test scope via `super::*`).

### 2. `policies/x_cast_gate.rs` — depth guard first, reorder sweep last

- Import: `use super::context::{PolicyContext, SearchDepth};` (extend the existing
  `use super::context::PolicyContext;` at `:39`).
- In `gate_rejects` (`:187`), **prepend** as the first statement:
  ```rust
  // Perf: the board-wide `max_x_value` affordability sweep below only needs to
  // be correct at the committed decision. In beam/rollout lookahead an X=0 cast
  // is already dominated by its resulting-state eval, so scoring it neutral there
  // costs nothing and removes the per-node sweep that regressed large boards.
  if !ctx.at_root() {
      return None;
  }
  ```
- **Reorder** the remaining body so the card-local payoff walk precedes the board-wide sweep. After
  the existing action-match + `{X}`-shard extraction block (`:189-228`, unchanged), replace the
  current "`max_x_value` first, then payoff walk" (`:230-248`) with:
  ```rust
  // Card-local payoff walk FIRST (no board sweep): if the payoff is not a
  // guaranteed no-op at X=0 (fixed meaningful residual, or does not scale with
  // X), the gate never fires — skip the affordability sweep entirely.
  let (effects, object_level_x) = payoff_effects_and_x_ref(ctx, object_id, ability, is_spell);
  if !no_op_at_x_zero(state, ctx.ai_player, object_id, ability, &effects, object_level_x) {
      return None;
  }

  // Board-wide affordability sweep LAST, only for genuine no-op-at-X=0 payoffs
  // at the root. CR 601.2b/f: `max_x_value` caps X to the legally payable max;
  // max >= 1 means the ramp path (`XValuePolicy`) handles it — don't gate.
  if max_x_value(state, ctx.ai_player, x_manacost, Some(object_id)) != 0 {
      return None;
  }

  Some(PolicyReason::new("x_cast_zero_no_op").with_fact("max_x", 0))
  ```
  (Net: same verdict for all inputs; `max_x_value` now runs only when at root **and** the payoff is a
  no-op-at-X=0.)
- Tests: update the two ctx-building helpers `verdict_for` (`:576-593`) and
  `verdict_for_cast_with_facts` (`:540-574`) to set `search_depth: SearchDepth::Root`. Add the two
  new tests from Claims A & (unit part of) C:
  - `helix_pinnacle_lookahead_returns_neutral` — Helix fixture, zero mana, build ctx via a new
    `verdict_for_activate_at` helper (or parameterize the existing `verdict_for_activate`/`verdict_for`
    to take a `SearchDepth`) with `SearchDepth::Lookahead` → `assert_not_reject`.
  - (search-level test lives in `search.rs`, step 5.)

### 3. `planner/mod.rs` — thread depth through `tactical_score`, `policy_priors`, and interior/rollout construction

- `use crate::policies::context::{PolicyContext, SearchDepth};` (extend existing import).
- `tactical_score` (`:908`): add trailing param `search_depth: SearchDepth`; set it in the
  `PolicyContext` literal at `:923-931` (`search_depth,`).
- Interior caller `search_value` (`:1108`): pass `SearchDepth::Lookahead`
  (`|candidate| services.tactical_score(state, &ctx, candidate, scoring_player, SearchDepth::Lookahead)`).
  Rationale: every `search_value` frame is ≥1 ply below the decision root (entered from
  `evaluate_after_action` after one applied candidate).
- `policy_priors` (`:954`): add trailing param `search_depth: SearchDepth`; forward it to
  `self.policies.priors(..., search_depth)`.
- `planner_evaluation` (`:979`): call `self.policy_priors(state, &ctx, &ctx.candidates,
  scoring_player, SearchDepth::Lookahead)` — this path is reached only from `rollout_estimate`
  (deep lookahead).
- Test callers: `policy_priors` @ `:1493` → `SearchDepth::Lookahead` (it exercises the rollout
  prior path). Any `tactical_score` test callers in `planner/mod.rs` (compiler will list) →
  `SearchDepth::Root` unless the test specifically models lookahead. Update the three test
  `BeamContinuationPlanner`/`PolicyContext` construction helpers as the compiler flags them.

### 4. `policies/registry.rs` — thread depth through `priors`

- `use super::context::{PolicyContext, SearchDepth};` (extend existing import).
- `priors` (`:457`): add trailing param `search_depth: SearchDepth`; set `search_depth,` in the
  `PolicyContext` literal at `:474-482`.
- Test callers `:654, :679` and `x_value.rs:363/483/531` → pass `SearchDepth::Lookahead` (these test
  the rollout prior path; the exact value is immaterial to `XValuePolicy`/`CopyValuePolicy` which do
  not read `search_depth`, but `Lookahead` matches the semantic they model).

### 5. `search.rs` — set Root at the two root scorers, the trace, and add the end-to-end test

- `use crate::policies::context::SearchDepth;` (or via existing `PolicyContext` import path).
- Root ranking `:1786`: `services.tactical_score(state, &ctx, &g.candidate, ai_player, SearchDepth::Root)`.
- Heuristic-only `:1886`: `... &candidate.candidate, ai_player, SearchDepth::Root)`.
- `emit_trace_for_candidate` `:598-606` `PolicyContext` literal: `search_depth: SearchDepth::Root,`
  (it traces the committed decision).
- Add the search-level discriminating test `xcast_zero_no_op_not_committed_at_root` (+ its
  reach-guard sibling with X≥1 mana) per Claim C, using the `score_candidates_core` harness pattern
  at `:2915`.

### 6. `tactical_gate.rs` — set Root on the pre-search gate context

- `gate_candidates` `:111-119` `PolicyContext` literal: `search_depth: SearchDepth::Root,`
  (`gate_candidates` runs only on the root candidate set; it does not invoke the registry, so the
  value is semantically-correct documentation rather than behavior-affecting, but the field is
  mandatory to compile). Import `SearchDepth`.

### 7. Remaining test construction sites (compiler-enumerated)

Every other `PolicyContext { ... }` literal is in test code: the per-policy `#[cfg(test)]` modules
(`aggro_pressure`, `anti_self_harm`, `board_wipe_telegraph`, … — see the grep list) and the
`policies/tests/*.rs` fixtures (`artifact_synergy`, `blink_payoff`, `enchantments_payoff`,
`equipment_payoff`, `lifegain_payoff`, `reanimator_payoff`). Add `search_depth: SearchDepth::Root,`
to each (Root is the correct default — tests model a committed decision). This is a mechanical
compile-fix; `cargo fmt --all` then Tilt `test-ai` confirms completeness (a missed site is a hard
compile error, so there is no silent gap).

---

## Verification cadence (risk-scaled; AI/policy floor is escalated)

1. `cargo fmt --all` (always direct).
2. Tilt: `./scripts/tilt-wait.sh clippy test-ai` (fall back to direct phase-ai clippy/test only if
   Tilt is down: `tilt get uiresource clippy >/dev/null 2>&1`). All new/updated unit + search-level
   tests green.
3. **`cargo ai-gate` (paired-seed report, mandatory).** This is a behavior change (lookahead
   move-ordering shifts), so decision flips are possible. Attach the paired-seed report. Any W→L
   flip requires a written rationale; **do not refresh baselines without the paired-seed report**
   (escalated floor = `changes_requested`). Expected: near-zero flips (root decision is bit-identical;
   only interior move-ordering changes).
4. **Performance acceptance gate (the whole point):**
   - `cargo ai-perf-gate` — run and attach. Note: `max_x_value` bumps **no** `PerfCounterSnapshot`
     field, so `ai-perf-gate` is structurally blind to this specific saving (per the skill's
     Performance §4). It is necessary but not sufficient.
   - **Wall-clock A/B (mandatory, decisive):** on a large late-game `{X}`-heavy board (the skill
     cites the turn-41 Court-of-Grace class; construct/reuse an equivalent `{X}`-dense fixture),
     measure decision wall-clock **before vs after** across card-mix (at least: an X-scaled mana-sink
     like Exsanguinate/Helix **and** a fixed-residual `{X}` card, to show recovery across the class,
     not just the reorder's narrow subclass). Attach the numbers; the acceptance criterion is the
     `3de827350d` regression recovered (interior/rollout sweeps eliminated).
5. Do **not** run `cargo build/clippy/test` directly while Tilt is up (target-lock contention).

## Out of scope (explicit)

No engine files (`casting_costs.rs` untouched); no `can_afford_x_at_least_one` primitive (deferred,
justified); no changes to `x_reference.rs` / `x_value.rs` semantics; no display-derive revival in sim
mode; no other policy's behavior; `mtgish` untouched.
