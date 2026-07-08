# Pipeline 5 (Round 2) — Transposition Table + Iterative Deepening in phase-ai Search

> Round-2 revision. The round-1 plan (`pipeline5-tt-id-plan.md`) is the base; a
> fresh reviewer confirmed its verified-correct parts (analogous trace,
> determinism plumbing, per-decision `PlannerServices` placement, `TtBound` typed
> enum, per-rung budget reset, scope, ai-gate protocol, cross-decision-TT
> deferral). This document supersedes it and closes the 4 reviewer-verified gaps:
> **GAP 1** (TT-hash omissions), **GAP 2** (`Keyword: !Hash` + misattributed
> rationale), **GAP 3** (Test #3 not implementable), **GAP 4** (rung-0 narrative
> inconsistency). Every source line below was re-verified against the working
> tree during this revision.

## Goal restatement

The policy-guided beam alpha-beta search in `phase-ai` commits to a **fixed**
depth up front and, when the 1500 ms wall-clock deadline fires mid-search,
**collapses every not-yet-searched candidate to a tactical-only score** (`r.score
* tactical_weight`, `search.rs:1702-1705`). It also re-searches identical
positions from scratch (siblings that transpose), because the only memo is a
256-entry **value-only** eval cache (`planner/mod.rs:509-519`) with no
depth/bound semantics.

This pipeline delivers:
- **(a) A transposition table (TT)** — memoizes interior search-node *values with
  alpha-beta bound + depth semantics*, keyed on a position hash, scoped to a
  single decision. On a sufficient-depth hit it returns immediately, skipping the
  whole subtree re-search.
- **(b) Iterative deepening (ID)** — depth `0 → 1 → … → max` under the existing
  deadline, returning the **deepest fully-completed iteration's** scores instead
  of the tactical-only collapse. Measurement mode pins the iteration ceiling
  (never consults the wall clock) so byte-determinism holds.

All work is confined to `crates/phase-ai/src/{planner/mod.rs, search.rs,
config.rs}`. Combat AI (`search.rs ~1502-1515`, a greedy bypass) is untouched.

---

## Analogous Trace (hard gate) — unchanged from round 1, re-verified

The feature most similar to what we are building is the **existing eval cache +
shared `Deadline` bail path**, traced end-to-end:

- **Config source of the deadline** — `config.rs:20` `AI_SEARCH_TIME_BUDGET_MS =
  Some(1500)`; consumed at `config.rs:110` (`SearchConfig.time_budget_ms`).
- **Deadline construction** — `planner/mod.rs:364-370` (`PlannerServices::new`)
  gates on `execution_mode.is_measurement()`: measurement ⇒ `Deadline::none()`,
  interactive ⇒ `Deadline::after(ms)`. Mirrored onto `AiContext` (`:375-376`).
  **Re-verified this revision.**
- **Node/time budget primitive** — `SearchBudget` (`planner/mod.rs:29-71`):
  `exhausted()` = `nodes_evaluated >= max_nodes || deadline.expired()`.
  Constructors: `new(max_nodes)` (deadline none), `with_time_limit(max_nodes,
  Duration)`, `with_deadline(max_nodes, Deadline)` (`:56`, takes a `Copy`
  `Deadline` by value). **Re-verified.**
- **Value memo** — `eval_cache: HashMap<u64,f64>` (`planner/mod.rs:331`), keyed by
  bare `quick_state_hash` (`evaluate_state_cached`, `:509-519`), **capped at 256
  entries** (`if self.eval_cache.len() < 256`, `:515`), value-only. **The cap is
  load-bearing for GAP 1 — see Identity/Provenance.**
- **Candidate memo** — `candidate_cache: HashMap<u64, Arc<AiDecisionContext>>`
  (`:339`), keyed by `candidate_cache_key = quick_state_hash + hash_waiting_for`
  (`:228-233`). Populated by `build_decision_context` (`:408-425`), which is the
  **sole** candidate-generation entry inside search (`search_value` calls it at
  `:866`). **This cache is UNCAPPED** (plain `insert` at `:422-423`, no `.len()`
  guard) — also load-bearing for GAP 1.
- **The search itself** — `BeamContinuationPlanner::search_value`
  (`planner/mod.rs:849-925`): alpha-beta minimax at fixed `self.depth`, ordered by
  `rank_candidates` (`:963-985`), leaf (`depth==0`) = `rollout_estimate` (`:860`);
  interior falls back to `evaluate_state_quiesced` on empty candidates / budget
  exhaustion (`:862-874`, `:920-924`). Mid-loop deadline bail at `:898-900`.
- **Search entry** — `score_candidates_with_session` (`search.rs:1483-1740`):
  builds `PlannerServices` (`:1517`), builds `SearchBudget` (`:1626-1635`), one
  `build_continuation_planner` (`:1637`, depth = `max_depth-1`), ranks top-level
  candidates (`:1673-1692`), walks them (`:1699-1719`) calling
  `planner.evaluate_after_action` — **this is the exact loop ID restructures**,
  and `:1702-1705` is the collapse we remove. Scoring formula per candidate:
  `continuation_score + (r.score * tactical_weight)` on success (`:1707-1709`),
  `r.score - 1000.0` on failed simulation (`:1716`), `r.score * tactical_weight`
  on deadline collapse (`:1705`). Measurement mode re-sorts by `action_order_key`
  (`:1721-1723`). **All re-verified this revision.**
- **The `Deadline` type** — `engine/src/util/deadline.rs`: `after(ms)` (limit =
  `Instant::now() + ms`), `none()` (const, `limit: None`, never expires),
  `expired()` (`Instant::now() >= limit`), `remaining()`. `#[derive(Copy)]`.
  `web_time::Instant` ⇒ works native + WASM.

The TT extends the eval-cache pattern (same struct location, same per-decision
scope, same "hash a position → memoize" shape) with depth+bound fields. ID
restructures the single-pass candidate walk into a depth loop over the same
`BeamContinuationPlanner`. **Nothing new architecturally — both compose from the
traced primitives.**

---

## Applicable skills

No workspace skill maps cleanly (this is neither a new engine effect, parser
pattern, keyword, trigger, static, replacement, nor an interactive
WaitingFor/GameAction round-trip). The closest is **`/add-ai-feature-policy`**,
but that governs `DeckFeatures` axes + `TacticalPolicy` wiring — this pipeline
touches neither. It is a **search-engine internal change**, so the governing
document is CLAUDE.md's design principles (building blocks,
parameterize-don't-proliferate, no bool flags, exhaustive match) plus the AI-gate
protocol. Confirmed no skill checklist applies; none is skipped. No new engine
enum variant is added, so `/add-engine-variant` does not apply (`TtBound` is a
`phase-ai`-local type — see Variant Discoverability).

---

## Architectural sections

### Pattern Coverage
This is not card-scoped — it changes the **search algorithm** used for *every*
non-combat, non-deterministic AI decision at `search.enabled` difficulties
(Medium, Hard, VeryHard, CEDH, plus WASM/multiplayer-scaled variants). It affects
100% of beam-searched decisions across all decks/formats. There is no "class of
cards" axis; the coverage axis is **every search-enabled decision**, so the
build-for-the-class principle is satisfied at the algorithm level (one TT + one ID
loop serve all positions, no position-specific special-casing).

### Building Blocks (compose, don't reinvent)
- `quick_state_hash` (`planner/mod.rs:95-215`) — reused as the **base** of the new
  `search_position_hash` (folded in as the first digest, then extended — see
  Identity/Provenance).
- `hash_json_value` (`planner/mod.rs:240-282`) — the **existing** private
  serde-value folder (canonical-object-key-sorted, deterministic). Already used by
  `hash_waiting_for` (`:235-238`). **Reused** to fold the non-`Hash` types
  (`Vec<Keyword>`, `Vec<CommanderDamageEntry>`, `StackEntry`) into
  `search_position_hash` — no `Hash` derive is added to any shared engine type
  (GAP 2 resolution).
- `candidate_cache_key`'s composition idiom (`:228-233`) — the TT key mirrors it:
  `search_position_hash + hash_waiting_for`, so acting-player/decision-type is in
  the key (a maximizing node and a minimizing node at the "same board" never
  alias).
- `engine::util::Deadline` — the ID stop signal; **no new time primitive**. Its
  `none()` sentinel (never expires) is the existing precedent for a
  "disabled-capability via typed value" pattern that this plan does **not** need
  to extend.
- `SearchBudget::with_deadline` (`:56`) — reused per iteration, sharing the one
  `services.deadline` (which is already `none()` in measurement, so one uniform
  constructor serves both modes — no `is_measurement` match needed at the call
  site).
- `BeamContinuationPlanner` + `evaluate_after_action` (`:928-948`) — reused
  verbatim as the per-iteration searcher; ID just varies its `depth` field.
- `eval_cache` (`:509-519`) — **kept** (leaf static-eval memo); the TT is a
  *separate* interior-node memo (different semantics), not a replacement.
- **One new helper justified**: `search_position_hash(state)` — a strict
  **superset** of `quick_state_hash`'s field dependency, adding exactly the axes a
  *bound-returning* TT cannot tolerate aliasing on that are **not** already
  protected by the search's own caches. Full justification (which axes, and why
  each is hashed vs. argued-safe) in Identity/Provenance below.

### Logic Placement
- `search_position_hash`, `transposition_key`, `TtEntry`, `TtBound`, the TT field
  + `tt_hits` witness, `tt_probe`/`tt_store`, and the TT integration inside
  `search_value` → **`planner/mod.rs`** (the search engine owns search
  memoization, exactly where `eval_cache` and `search_value` already live).
- The ID loop → **`search.rs` `score_candidates_with_session`** (the decision
  entry that already owns budget construction, candidate ranking, and the
  now-removed collapse). ID is an *orchestration* concern over
  `evaluate_after_action`, so it belongs at the call site, not inside the planner.
- No `config.rs` behavior change. **Optional (recommended)**: a doc-comment update
  at `config.rs:9-16` noting the deadline now bounds ID rungs. No new config field
  — the ID ceiling is derived from the existing `max_depth`/`planner_mode`,
  satisfying parameterize-don't-proliferate.

### Rust Idioms
- `TtBound` is a **typed enum** `{ Exact, LowerBound, UpperBound }` — never a bool
  pair (no-bool-flags rule). Matched **exhaustively** in the probe.
- `TtEntry { depth: u32, value: f64, bound: TtBound }` is `Copy` (no clone cost, no
  stored `CandidateAction`).
- Iteration ceiling derived by exhaustive `match config.search.planner_mode`
  (`BeamOnly => 0`, `BeamPlusRollout => max_depth.saturating_sub(1)`), mirroring
  the existing `build_continuation_planner` match (`:950-961`) — no wildcard.
- `saturating_sub` for depth arithmetic (mirrors existing `:957`).
- The TT probe returns `Option<f64>` (`Some` = cutoff hit) — idiomatic early-out,
  no sentinel values.
- Non-`Hash` engine types are folded through the **existing** serde helper rather
  than by deriving `Hash` on the shared type — no new trait bound proliferation,
  no multi-agent-safety risk on `keywords.rs`.

### Nom Compliance
**N/A — justified.** No file under `crates/engine/src/parser/` is touched. This is
an AI search-engine change with zero Oracle-text parsing.

### CR Annotations
**N/A — justified.** AI-layer heuristics (search depth scheduling, memoization)
implement no Comprehensive Rules behavior — they are decision *quality*
optimizations over an already-rules-correct `apply_as_current_for_simulation`. No
`// CR` annotations are added. (Existing CR comments in `search.rs` are untouched.)

### Extension vs Creation
**Extension.** The TT extends the eval-cache/`search_value` memoization pattern;
ID extends the existing single-pass candidate walk into a depth loop over the same
planner. The only genuinely new types are `TtEntry`/`TtBound` (phase-ai-local
value types) and the `search_position_hash`/`transposition_key` helpers. Both
compose from existing primitives.

### Variant Discoverability
No new variant is added to any engine type enumerated by `/add-engine-variant`
(`QuantityRef`, `Effect`, `TargetFilter`, `Keyword`, …). `TtBound` lives in
`phase-ai`, not `engine`, and models an internal alpha-beta bound classification
(textbook TT), not an MTG game concept. `cargo engine-inventory` is therefore not
consulted (it inventories the engine surface, which is unchanged). Notably, this
plan also does **not** add `Hash` to `Keyword` (GAP 2) — the shared engine enum is
left byte-identical; folding happens locally in `phase-ai`.

### Identity / Provenance Contract — TT hash correctness (the critical section, fully rewritten for GAP 1 + GAP 2)

**The concern (from the audit):** a wrong TT hit changes search *results* (an
interior node returns a cached value and its whole subtree is skipped), whereas a
wrong eval-cache hit only perturbs one leaf's static score. So the TT key must not
*systematically* alias two positions the search would value differently.

**The two-cache asymmetry that drives every disposition below (verified):**
The search already has two per-decision memos whose keys are subsets of the TT
key's field dependency:
1. **`candidate_cache`** — keyed by `candidate_cache_key = quick_state_hash +
   hash_waiting_for`. **UNCAPPED** (`build_decision_context`, `:408-425`, plain
   `insert`, no eviction). Sole candidate-generation entry inside `search_value`
   (`:866`).
2. **`eval_cache`** — keyed by `quick_state_hash`. **CAPPED at 256**
   (`evaluate_state_cached`, `:515`). Sole leaf static-eval memo.

Define `search_position_hash` so its **field dependency is a strict superset** of
`quick_state_hash`'s. Then (modulo random 64-bit collision — the standard Zobrist
tolerance, ~1e-16 within a few-hundred-entry table) `transposition_key`'s
dependency ⊇ `candidate_cache_key`'s dependency. Two consequences:

- **Candidate-generation-gating axes are covered by the UNCAPPED
  `candidate_cache` for free.** If two positions share `transposition_key`, they
  share `candidate_cache_key`, so `build_decision_context` returns the **same
  cached candidate set** for both (no eviction can break this — the cache is
  uncapped). Their children are therefore generated identically, so the TT
  introduces **no candidate-set aliasing that production doesn't already accept
  today.** Any field read *only* by candidate generation (via `casting.rs`
  candidate enumeration) needs **no** additional hashing — the superset argument is
  airtight for it.
- **Eval-leaf-read axes are NOT covered, because `eval_cache` is CAPPED.** After
  256 entries, `evaluate_state_cached` bypasses the cache and calls
  `evaluate_with_strategy` on the **real** `GameState`, reading fields directly
  (not through `quick_state_hash`). So two positions sharing `quick_state_hash`
  but differing in an un-hashed eval-relevant field can produce **different eval
  values** post-eviction, one of which could be stored in the TT and wrongly
  reused. Any field read by `evaluate_state` / `zone_bonus` / `rollout` /
  `tactical_score` must therefore be **hashed into `search_position_hash`.**

A third, cache-independent failure mode also requires hashing:
- **Post-`apply` divergence.** If a field is neither in candidate generation nor in
  leaf eval, but two sibling in-search actions can produce board-identical (under
  `quick_state_hash`) children that *diverge only after the stack resolves*, then
  the parent node's `transposition_key` collides while its true subtree value
  differs. Neither cache mediates this (the divergence is in `apply_candidate`'s
  output, not in the candidate set or a leaf eval). Such fields must be hashed.

**Axis-by-axis disposition (each verified against source):**

| Axis | Read by | Cache that would cover it | Verdict |
|------|---------|---------------------------|---------|
| Per-object **keywords** (`obj.keywords`) | Leaf eval → `creature_combat_value` (`eval.rs:531`, `has_keyword(&Keyword::Flying/Trample/Deathtouch/Lifelink/…)`), which feeds `tactical_score` nudges + rollout policy priors | `eval_cache` (CAPPED) — not reliable | **HASH** (via serde-fold; GAP 2) |
| **`summoning_sick`** (a.k.a. entered-battlefield-this-turn) | Leaf eval → `zone_bonus`→`available_mana` (`zone_eval.rs`, via `combat::has_summoning_sickness(obj)` → `obj.summoning_sick`, `combat.rs:2892-2900`) | `eval_cache` (CAPPED) — not reliable | **HASH** (per-object bool) |
| **`commander_damage`** (`Vec<CommanderDamageEntry>`) | Leaf eval → commander threat (`eval.rs:244-256`, `commander_lethal_headroom`) in 3+ player games | `eval_cache` (CAPPED) — not reliable | **HASH** (serde-fold; entry is `Serialize`, not `Hash`) |
| **Stack entry targets/modes** (`StackEntry.kind`, `:5089-5094`) | Neither candidate-gen nor leaf eval — materializes **after** the stack resolves via `apply_candidate` | Neither cache | **HASH** (serde-fold; post-apply divergence, e.g. "Shock→A" vs "Shock→B" both leave `source_id`+`controller` identical) |
| **Exile contents** (`exile: im::Vector<ObjectId>`) | Candidate gen (cast-from-exile: foretell/impulse/adventure, `casting.rs:665-703` reached via `build_decision_context`) | `candidate_cache` (UNCAPPED) — superset holds | **HASH** anyway (cheap: small zone; mirrors graveyard treatment; defense-in-depth against a future `candidate_cache` cap. Matches reviewer's required shape.) |
| **Library order** (per player) | Candidate gen (cast-from-top: Future Sight/Bolas's Citadel) via `candidate_cache`; draws within horizon move cards to hand, which `quick_state_hash` already hashes by ObjectId | `candidate_cache` (UNCAPPED) — superset holds | **HASH** (kept from round 1: cost is only per-player `O(library)` u64 hashes ≈ 120 ops/node, negligible vs the `apply()` clone; hashing removes reliance on the cache for top-card-visibility lines) |
| **Per-turn cast-permission used-sets** (`graveyard_cast_permissions_used`, `graveyard_cast_permissions_used_per_type`, `hand_cast_free_permissions_used`, `exile_play_permissions_used`, `exile_play_single_use_consumed`, `exile_cast_permissions_used`, `top_of_library_cast_permissions_used`, `cards_exiled_with_source_this_turn`; `game_state.rs:6481-6557`) | Candidate gen **only** (they gate which cast-from-zone candidates `casting.rs` enumerates); not read by `eval.rs` | `candidate_cache` (UNCAPPED) — superset holds | **OMIT** — rely on the superset argument. Rigorous: equal `transposition_key` ⇒ equal `candidate_cache_key` ⇒ identical **cached** candidate set for both positions (no eviction to break it), so the TT never aliases two used-set-divergent positions into *different* children. No new aliasing beyond what production `candidate_cache` already accepts. |
| `poison_counters`, `player_counters`, `attached_to` | Unread by both eval and candidate-gen (reviewer-confirmed) | n/a | **OMIT** (confirmed safe) |

**Deviation from round 1, explicitly flagged for the reviewer:** round 1 claimed
"`search_position_hash = quick_state_hash + keywords + library order` has no
systematic omission." That was false (GAP 1). This revision adds **exile
contents, `summoning_sick`, `commander_damage`, and stack targets/modes**, and
provides the two-cache asymmetry as the *proof* that the used-sets and library
order are safe under the superset argument rather than an unbacked assertion. The
`kw.hash()` call round 1 proposed **would not compile** (`Keyword: !Hash`) and is
replaced by serde-folding (GAP 2).

**Why keywords belong in the key — corrected rationale (GAP 2):** the round-1
claim that `board_stats`/the eval-cache leaf reads battlefield keywords was
misattributed. The real dependency: `creature_combat_value` (`eval.rs:531`) reads
keywords via `has_keyword(&Keyword::Flying)` etc., and that value flows into
`tactical_score` move-ordering nudges **and** rollout policy priors — not into the
`board_stats` leaf. Combined with the `eval_cache` cap, that read is not reliably
memoized, so keywords must be in the TT key.

**`Keyword`/`CommanderDamageEntry` folding — the compile-correct path (GAP 2):**
`Keyword` derives `Debug, Clone, PartialEq, Eq, Serialize` only (`keywords.rs:477`)
— **no `Hash`**, and it is used in no `HashSet`/`HashMap` (verified). Its variants
carry non-trivially-`Hash` payloads (`TargetFilter` in `Enchant`, `ManaCost`,
`ProtectionTarget`, `WardCost`, …). Deriving `Hash` would require auditing every
payload type for `Hash` and would edit a hot shared file (multi-agent-safety
risk). Instead **fold locally in `phase-ai`** via the existing `hash_json_value`
serde folder: `hash_json_value(&serde_json::to_value(&obj.keywords).expect("keywords
serialize"), &mut hasher)`. This is (a) lossless (Debug/serde capture discriminant
+ payload), (b) deterministic (serde array order = slice order; object keys sorted
by `hash_json_value`), (c) the same technique already proven for `hash_waiting_for`,
(d) zero edits to the shared `Keyword` type. `CommanderDamageEntry` (`Serialize`,
not `Hash`, `game_state.rs:894`) is folded the same way. **Cost containment:** each
serde-fold is guarded by an `is_empty()` check, so keyword-less objects (vanilla
tokens — the 1000-token latency worst case, row 8) and non-commander games skip the
allocation entirely; realistic keyword-bearing boards are small.

**Binding / lifetime contract of a TT entry (unchanged from round 1):**
- **Authority type & id:** key = `u64` (`search_position_hash + hash_waiting_for`);
  value = `TtEntry { depth, value, bound }`.
- **Binding time:** written when `search_value` returns a *non-truncated* result
  (budget-truncation guard, Step 5). Read at `search_value` entry.
- **Live vs snapshot:** the stored `value` is a **snapshot** of the minimax value
  at the stored `depth` under the bound. It is only *reused* when `entry.depth >=
  remaining_depth` **and** the bound proves an alpha-beta cutoff for the current
  `(alpha, beta)` window — otherwise ignored (never blindly returned), which keeps
  it sound across ID rungs with different windows.
- **Storage:** `transposition_table: HashMap<u64, TtEntry>` on `PlannerServices`.
- **Consuming fn:** `PlannerServices::tt_probe` / `tt_store`, called only from
  `BeamContinuationPlanner::search_value`.
- **Invalidation / expiration:** **per-decision.** `PlannerServices` is constructed
  fresh in every `score_candidates_with_session` (`search.rs:1517`), so the table
  is empty at decision start and dropped at its end — no cross-turn staleness.
- **Multi-authority hostile fixture (proves the binding):** two positions that are
  board-identical *except* one has a creature with a granted keyword (flying).
  `quick_state_hash` returns **equal**; `search_position_hash` returns **distinct**.
  Test asserts the distinction (revert-failing: drop the keyword fold ⇒ collide ⇒
  fail). Sibling fixtures for `commander_damage`, stack targets, and exile contents
  are added the same way (Verification Matrix rows 1, 1b–1d).

**Cross-decision / turn-scoped TT — dispositioned (unchanged): deferred, not
built.** Within-decision only. Rationale: (1) ID's re-search overhead is entirely
intra-decision, so a per-decision TT captures ~all of the ID win; (2) between
decisions the state has advanced one applied action, so cross-decision hit rate is
low while staleness risk is real; (3) it would require threading turn-scoping into
`AiSession` (the `projection_cache` pattern at `session.rs:46,167-199` is the
template) for marginal benefit. If future profiling shows high cross-decision
transposition, promote the table onto `AiSession` with a `turn_number +
active_player`-scoped key like `ProjectionKey`. Follow-up, not in scope.

---

## Determinism contract (hard constraint — measurement mode) — unchanged, re-verified

`ExecutionMode::Measurement` must be a pure function of `(binary, config, seed)`.
ID introduces wall-clock-dependent depth **only in interactive mode**:

- `PlannerServices::new` sets `deadline = Deadline::none()` when `is_measurement()`
  (`planner/mod.rs:364-370`, verified). `Deadline::none().expired()` is **always
  false**.
- The ID loop's only wall-clock consultation is `services.deadline.expired()`. In
  measurement that is constant `false`, so the loop **always runs the full fixed
  ceiling** `0..=iteration_ceiling(config)` and returns the deepest rung.
- The per-rung `SearchBudget` in measurement is
  `SearchBudget::with_deadline(max_nodes, services.deadline)` where
  `services.deadline` is `none()` ⇒ `exhausted()` is a pure function of
  `nodes_evaluated` — deterministic. (One uniform constructor; see Step 6.)
- **Therefore in measurement, depth is pinned by `config` alone; the clock is never
  read.** Asserted by `measurement_mode_deadline_is_none_and_ceiling_is_pinned`.

Interactive mode is the only place `expired()` returns `true`, and there it only
ever *stops early and returns an already-completed rung* — it never feeds the clock
into a stored value or a score.

**Determinism scope — WITHIN-PROCESS only (pre-existing cross-process caveat,
per pipeline-4's empirical finding):** the AI decision *trajectory* is **not**
cross-process deterministic today, for reasons pre-dating this change. Root cause:
engine-wide `std::collections::HashSet` with default `RandomState` (per-process
random seed) — e.g. `LayersDirty::EnteredObjects(HashSet<ObjectId>)`
(`game_state.rs:2180`) drives flush coalescing (`layers.rs:1896`), and HashSet
iteration order can leak into AI action tie-breaking. Evidence: two cold processes
with identical `(binary, card-data, seed, action-cap)` diverge on 7/19 perf
counters (e.g. `state_clone_for_legality` 4622 vs 4564 = different trajectories),
whereas two runs **within one process** are trajectory-identical (18/19 counters
byte-equal). Consequences for this plan:
1. The measurement-mode determinism **claim and its test are scoped to
   same-process repeated runs.** A cross-process byte-equality assertion would
   flake for this pre-existing engine reason, unrelated to TT/ID. Stated
   explicitly so the reviewer does not read row 6 as a cross-process guarantee.
   The engine HashSet issue is tracked separately and **must not** be touched here.
2. **The TT introduces no *additional* nondeterminism.** `transposition_table:
   HashMap<u64, TtEntry>` is accessed **only by key** (`tt_probe`/`tt_store`, keyed
   `get`/`insert`) — it is **never iterated**, so its internal `HashMap` order can
   never leak into a chosen action. `tt_hits` is an order-independent counter. The
   TT value returned by a probe is a function of `(key, depth, alpha, beta, entry)`
   only. No new HashMap/HashSet iteration is added on any path that influences move
   selection; where an existing order could leak it is untouched. (If a future edit
   ever needs to *iterate* the table into a decision, switch it to `BTreeMap` or a
   sorted pass — not required by this plan.)
3. The paired ai-gate protocol is unaffected — it compares **win-rate**
   (statistical), not byte-exact trajectories, so cross-process trajectory jitter
   does not perturb it. No change to the ai-gate section.

---

## Step-by-step implementation

### Step 1 — `planner/mod.rs`: add `search_position_hash`
Directly after `quick_state_hash` (ends `:215`), add. Folds the base digest, then
the four newly-required axes (keywords, `summoning_sick`, `commander_damage`, stack
targets/modes) plus exile contents and library order (see Identity/Provenance for
the per-axis justification). Non-`Hash` types are folded via the existing
`hash_json_value` serde helper, guarded by `is_empty()` for cost:

```rust
/// Position hash for the transposition table. Field dependency is a strict
/// **superset** of `quick_state_hash`, adding the axes a *bound-returning* TT
/// cannot tolerate aliasing on that the search's own caches don't already
/// protect (a wrong TT hit skips a whole subtree, unlike a wrong eval-cache hit
/// which only perturbs one leaf). See the TT design notes for the per-axis
/// disposition and the two-cache (uncapped candidate_cache / capped eval_cache)
/// argument that makes the omitted axes safe.
pub fn search_position_hash(state: &GameState) -> u64 {
    let mut hasher = DefaultHasher::new();
    // Base: reuse the existing digest, then extend its field dependency.
    quick_state_hash(state).hash(&mut hasher);

    // Exile contents (cast-from-exile candidate gen reads these; quick_state_hash
    // hashes only exile.len()). Mirror the graveyard treatment.
    for &id in &state.exile {
        id.hash(&mut hasher);
    }

    // Full library ordering per player (top-of-library cast / draw-horizon lines).
    for player in &state.players {
        for &id in &player.library {
            id.hash(&mut hasher);
        }
    }

    // commander_damage: read by eval commander-threat in 3+ player games; the
    // eval_cache 256-cap means this eval-leaf read is not reliably memoized.
    // CommanderDamageEntry is Serialize but not Hash -> fold via the existing
    // serde helper. Empty in non-commander games -> skipped.
    if !state.commander_damage.is_empty() {
        hash_json_value(
            &serde_json::to_value(&state.commander_damage).expect("commander_damage serializes"),
            &mut hasher,
        );
    }

    // Stack entry targets/modes: NOT covered by either cache (post-apply
    // divergence — e.g. "Shock target A" vs "Shock target B" share source_id +
    // controller). Fold each entry via serde (kind carries the ResolvedAbility
    // with targets/modes). Empty stack (common) -> skipped.
    for entry in &state.stack {
        hash_json_value(
            &serde_json::to_value(entry).expect("stack entry serializes"),
            &mut hasher,
        );
    }

    // Per-battlefield-object: summoning sickness (available_mana eval leaf) and
    // keywords (creature_combat_value -> tactical nudges + rollout priors). Both
    // are eval-leaf reads under the capped eval_cache, so both must be hashed.
    for &obj_id in &state.battlefield {
        if let Some(obj) = state.objects.get(&obj_id) {
            obj.summoning_sick.hash(&mut hasher);
            if !obj.keywords.is_empty() {
                // Keyword is Serialize but not Hash (and carries non-trivially-Hash
                // payloads); fold locally via serde rather than editing the shared
                // engine type. Lossless + deterministic (serde array order = slice
                // order; hash_json_value sorts object keys).
                hash_json_value(
                    &serde_json::to_value(&obj.keywords).expect("keywords serialize"),
                    &mut hasher,
                );
            }
        }
    }

    hasher.finish()
}
```

Notes for the implementer:
- `hash_json_value` (`:240-282`) and `hash_waiting_for` (`:235-238`) are private to
  `planner/mod.rs`; `search_position_hash` is in the same module and may call them.
- `state.exile` is `im::Vector<ObjectId>` (`game_state.rs:5858`); `player.library`
  is `Vec<ObjectId>`; both element types are `Hash` (already hashed elsewhere).
- `obj.summoning_sick` is a `bool` field on `GameObject` (read by
  `combat::has_summoning_sickness`, `combat.rs:2892-2900`).
- **Do NOT derive `Hash` on `Keyword` or `CommanderDamageEntry`.** If a future
  reviewer insists on a derive, that is a separate shared-type change — stop and
  return to the lead (see Stop-and-return items).

### Step 2 — `planner/mod.rs`: add TT key + types
Near `candidate_cache_key` (`:228`), add:

```rust
/// TT key: stronger position hash + full `WaitingFor` payload, so a maximizing
/// node and a minimizing node at the same board never share an entry.
pub fn transposition_key(state: &GameState) -> u64 {
    let mut hasher = DefaultHasher::new();
    search_position_hash(state).hash(&mut hasher);
    hash_waiting_for(&state.waiting_for, &mut hasher);
    hasher.finish()
}

/// Alpha-beta bound classification of a stored search value (typed — never a
/// pair of bools).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TtBound {
    /// Exact minimax value (window was not cut).
    Exact,
    /// Fail-high: true value is >= `value` (node returned >= beta).
    LowerBound,
    /// Fail-low: true value is <= `value` (node never exceeded alpha).
    UpperBound,
}

#[derive(Debug, Clone, Copy)]
pub struct TtEntry {
    pub depth: u32,
    pub value: f64,
    pub bound: TtBound,
}
```

### Step 3 — `planner/mod.rs`: add TT state + witness to `PlannerServices`
Add fields after `eval_cache` (`:331`):

```rust
    /// Interior-node search-value memo with alpha-beta bound + depth semantics
    /// (distinct from `eval_cache`, a value-only *leaf* memo). Scope is the
    /// `PlannerServices` lifetime — one decision — so no cross-turn staleness.
    transposition_table: HashMap<u64, TtEntry>,
    /// Count of TT cutoffs served this decision. Not an engine perf counter
    /// (perf_counters.rs is out of scope); a local witness that a re-search was
    /// actually skipped, used by the TT-hit regression test.
    pub tt_hits: u32,
```

Initialize both in `PlannerServices::new` (`:377-386`): `transposition_table:
HashMap::new(), tt_hits: 0,`.

Add a cap constant beside the code (mirroring the eval-cache 256 guard idiom):
`const TT_CAPACITY: usize = 4096;` — a decision searches ≤ `max_nodes` (≤96) ×
rungs interior nodes, so this never binds in practice; a defensive ceiling.

### Step 4 — `planner/mod.rs`: TT probe/store on `PlannerServices`
Add near `evaluate_state_cached` (`:509`):

```rust
/// Probe the TT. Returns `Some(value)` only when a stored entry proves an
/// alpha-beta cutoff for the current window at sufficient depth — otherwise
/// `None` (caller searches normally). Exhaustive match over `TtBound`.
fn tt_probe(&mut self, key: u64, depth: u32, alpha: f64, beta: f64) -> Option<f64> {
    let entry = *self.transposition_table.get(&key)?;
    if entry.depth < depth {
        return None; // shallower than we need — not trustworthy for this rung
    }
    let hit = match entry.bound {
        TtBound::Exact => Some(entry.value),
        TtBound::LowerBound if entry.value >= beta => Some(entry.value),
        TtBound::UpperBound if entry.value <= alpha => Some(entry.value),
        _ => None,
    };
    if hit.is_some() {
        self.tt_hits += 1;
    }
    hit
}

/// Store a search result. `alpha_orig`/`beta` classify the bound. Depth-preferred
/// replacement: keep a strictly deeper existing entry; otherwise insert (respecting
/// the capacity ceiling for new keys).
fn tt_store(&mut self, key: u64, depth: u32, value: f64, alpha_orig: f64, beta: f64) {
    let bound = if value <= alpha_orig {
        TtBound::UpperBound
    } else if value >= beta {
        TtBound::LowerBound
    } else {
        TtBound::Exact
    };
    match self.transposition_table.get(&key) {
        Some(existing) if existing.depth > depth => {} // keep the deeper entry
        _ if self.transposition_table.len() >= TT_CAPACITY
            && !self.transposition_table.contains_key(&key) => {} // cap guard
        _ => {
            self.transposition_table
                .insert(key, TtEntry { depth, value, bound });
        }
    }
}
```

### Step 5 — `planner/mod.rs`: wire the TT into `search_value` (`:849-925`)
Restructure the head so the probe happens *before* the expensive
`build_decision_context` + expansion, and the store happens on return with a
**budget-truncation guard** (never cache a value produced by an exhausted-budget
bail — a heuristic, not a true depth-`d` minimax value):

```rust
fn search_value(&self, state, depth, mut alpha, mut beta, services, budget) -> f64 {
    budget.tick();
    if depth == 0 {
        return services.rollout_estimate(state, self.rollout_depth); // leaf: not TT'd
    }
    if budget.exhausted() || matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
        return services.evaluate_state_quiesced(state);
    }

    let key = transposition_key(state);
    if let Some(v) = services.tt_probe(key, depth, alpha, beta) {
        return v; // re-search skipped
    }
    let alpha_orig = alpha;

    // ... unchanged expansion: build_decision_context (:866), empty-candidate
    // fallback (:873-875), rank (:880-884), alpha-beta loop (:894-918) ...

    let result = if best.is_infinite() {
        services.evaluate_state_quiesced(state)
    } else {
        best
    };
    // Budget-truncation guard: only memoize a fully-explored node. budget.exhausted()
    // includes deadline.expired() (:65), so a node that broke early on the mid-loop
    // deadline bail (:898-900) is NOT stored — only genuinely completed nodes enter.
    if !budget.exhausted() {
        services.tt_store(key, depth, result, alpha_orig, beta);
    }
    result
}
```

`depth == 0` leaves stay `rollout_estimate` (sampling-based, path-nudged — not a
clean position value, correctly excluded from the TT).

### Step 6 — `search.rs`: replace the fixed-depth walk with the ID loop (GAP 4 resolution)
In `score_candidates_with_session`, inside `if config.search.enabled {` (`:1622`),
**replace** the old budget construction (`:1626-1635`) and the walk
(`:1699-1720`, the loop with `deadline_hit`/tactical-collapse) with the following.

**GAP 4 decision — option (a): guard *every* rung at entry, including rung 0.**
Rationale (the deadline's purpose is a hard 1500 ms UX budget): if the deadline has
already expired when the loop begins (pathological — ranking itself consumed the
budget), we must do **zero** further `apply_candidate` work and return the
tactical-only baseline — *exactly* origin/main's collapse behavior (zero applies
past the deadline). Option (b) (unconditional rung 0) would run `branching`
`apply_candidate` clones + quiesced evals past an expired hard deadline, violating
the budget's intent even though `im::` makes each clone cheap. Under option (a) the
"no-regression floor == today's collapse" is genuinely returned when the deadline
pre-expires, and the hostile "even rung 0 can't complete" case is **reachable** (a
pre-expired deadline). Once rung 0 is *entered* (deadline not yet expired), it runs
atomically to completion — it is cheap (`branching` quiesced evals, no interior
search) — while rungs ≥ 1 may bail mid-rung and discard the partial:

```rust
let branching = config.search.max_branching as usize;

// ... is_target_selection / is_stack_response / tactical_weight (:1645-1666) unchanged ...
// ... ranked built + sorted + truncate(branching) (:1673-1692) unchanged ...

// Iterative deepening: rung 0 (quiesced eval per candidate) -> ceiling. Return the
// deepest *fully completed* rung.
let ceiling: u32 = match config.search.planner_mode {
    PlannerMode::BeamOnly => 0,
    PlannerMode::BeamPlusRollout => config.search.max_depth.saturating_sub(1),
};

// No-regression floor == origin/main's deadline collapse: tactical-only for every
// candidate. Overwritten by each completed rung; returned as-is only if not even
// rung 0 is entered (deadline pre-expired), which reproduces origin/main exactly.
let mut best_scored: Vec<(GameAction, f64)> = ranked
    .iter()
    .map(|r| (r.candidate.action.clone(), r.score * tactical_weight))
    .collect();

for iter_depth in 0..=ceiling {
    // Guard EVERY rung (incl. rung 0) at entry. Interactive: a pre-expired deadline
    // returns the tactical-only floor with zero applies (== origin/main). Measurement:
    // services.deadline is none() => never expires => full fixed ceiling => deterministic.
    if services.deadline.expired() {
        break;
    }
    // Fresh node budget per rung sharing the one services.deadline (which is none()
    // in measurement, so this single constructor is correct for both modes). The
    // deepest rung thus gets the full max_nodes just like origin/main's single pass.
    let mut budget = SearchBudget::with_deadline(config.search.max_nodes, services.deadline);
    let mut planner = BeamContinuationPlanner {
        depth: iter_depth,
        rollout_depth: config.search.rollout_depth,
    };

    let mut rung_scored = Vec::with_capacity(ranked.len());
    let mut completed = true;
    for r in &ranked {
        // Rungs >= 1 may bail mid-rung (interior search is expensive) and discard the
        // partial. Rung 0 is cheap (branching quiesced evals) and runs atomically once
        // entered, so it is never left partial.
        if iter_depth > 0 && services.deadline.expired() {
            completed = false;
            break;
        }
        let score = if let Some(sim) = apply_candidate(state, &r.candidate) {
            let cont = planner.evaluate_after_action(&sim, &mut services, &mut budget);
            cont + (r.score * tactical_weight)
        } else {
            r.score - 1000.0 // failed simulation — same penalty as origin/main (:1716)
        };
        rung_scored.push((r.candidate.action.clone(), score));
    }

    if completed {
        best_scored = rung_scored; // deepest completed rung so far
    } else {
        break;
    }
}

let mut out = best_scored;
if config.execution_mode.is_measurement() {
    out.sort_by_cached_key(|(action, _)| action_order_key(action));
}
out
```

Implementation notes:
- Remove the obsolete `budget` construction (`:1626-1635`) and the `deadline_hit`
  bookkeeping (`:1700`, `:1703`, `:1720`). `is_target_selection` /
  `is_stack_response` / `tactical_weight` (`:1645-1666`) and the `ranked`
  build/sort/truncate (`:1673-1692`) are unchanged.
- `SearchBudget::with_deadline` (`planner/mod.rs:56`) shares the one
  `services.deadline`, so the interior `budget.exhausted()` and the mid-loop bail
  (`planner/mod.rs:898`) agree on the same clock. This replaces the old
  `with_time_limit` (which minted a *second*, independent deadline).
- Add `BeamContinuationPlanner` (and, if not already fully qualified,
  `transposition_key`) to the `use crate::planner::{…}` list (`search.rs:28-30`),
  or reference fully-qualified. `PlannerMode` is already imported via
  `crate::config::…`.

**Node-cap disposition (explicit):** each rung resets the node budget to
`max_nodes`, so the *deepest* rung (the origin/main-equivalent) gets the **full**
`max_nodes` — never starved, so no depth regression at the target depth; the TT
only accelerates it. Total `apply()` across rungs ≤ `(ceiling+1) × max_nodes`, but
rung 0 does zero `search_value` ticks (`evaluate_after_action` at depth 0 returns
`evaluate_state_quiesced` directly) and rung 1 is tiny, so practical overhead is a
few extra evals, dwarfed by the deepest rung. The one-shared-budget alternative
was rejected: shallow rungs could consume the cap and starve the deepest rung below
origin/main depth — a direct win-rate regression risk.

### Step 7 — `config.rs` (optional doc-only)
Update the module doc at `config.rs:9-16` to note the wall-clock budget now bounds
*iterative-deepening rungs* (deepest completed rung returned on expiry) rather than
a single fixed-depth pass. No code/field change.

---

## Verification Matrix

Runtime tests colocated in `#[cfg(test)]` in `planner/mod.rs` and `search.rs`. Run
via `cargo test -p phase-ai` semantics (Tilt `test-ai` resource). Each row names
the seam, the revert-failing assertion, and its paired reach-guard. Timing-flaky
constructions are explicitly avoided — every deadline-dependent assertion uses a
deterministic `time_budget_ms = Some(0)` pre-expiry (see rows 5a/5b rationale).

| # | Claim | Changed seam | Production entry | Test (add) | Revert-failing assertion | Reach-guard / sibling / hostile |
|---|-------|-------------|------------------|-----------|--------------------------|-------------------------------|
| 1 | Stronger hash distinguishes a **keyword-only** difference `quick_state_hash` aliases | `search_position_hash` (keyword fold) | TT key | `search_position_hash_distinguishes_granted_keyword` (planner) | `assert_ne!(search_position_hash(a), search_position_hash(b))` for two states differing only by one battlefield object's `keywords` (a has flying) | **Reach-guard:** first `assert_eq!(quick_state_hash(a), quick_state_hash(b))` — proves the base hash *does* alias, so `_ne!` is non-vacuous. Revert (drop keyword fold) ⇒ equal ⇒ fails. Also proves the serde fold compiles (GAP 2 — `kw.hash()` would not). |
| 1b | Distinguishes **stack targets/modes** (post-apply divergence) | `search_position_hash` (stack fold) | TT key | `search_position_hash_distinguishes_stack_targets` (planner) | `assert_ne!` for two states whose single stack entry differs only in its target/mode | **Reach-guard:** `assert_eq!(quick_state_hash(a), quick_state_hash(b))` (only source_id+controller hashed there). Revert (drop stack fold) ⇒ collide ⇒ fail. |
| 1c | Distinguishes **commander_damage** | `search_position_hash` (commander fold) | TT key | `search_position_hash_distinguishes_commander_damage` (planner) | `assert_ne!` for two states differing only in a `CommanderDamageEntry` amount | **Reach-guard:** `assert_eq!(quick_state_hash(a), quick_state_hash(b))`. Sibling of row 1. |
| 1d | Distinguishes **exile contents** | `search_position_hash` (exile fold) | TT key | `search_position_hash_distinguishes_exile_contents` (planner) | `assert_ne!` for two states with different exile ObjectIds but equal `exile.len()` | **Reach-guard:** `assert_eq!(quick_state_hash(a), quick_state_hash(b))` (len-only there). |
| 2 | Stronger hash distinguishes **library ordering** | `search_position_hash` | TT key | `search_position_hash_distinguishes_library_order` | `assert_ne!` for two states with swapped top-two library ObjectIds | **Reach-guard:** `assert_eq!(quick_state_hash(a), quick_state_hash(b))` (length only). |
| 2b | **summoning_sick** is in the key | `search_position_hash` | TT key | `search_position_hash_distinguishes_summoning_sick` | `assert_ne!` for two states differing only by one battlefield creature's `summoning_sick` | **Reach-guard:** `assert_eq!(quick_state_hash(a), quick_state_hash(b))`. |
| 3 | **A TT hit skips a real re-search** (the core memoization win) — *services-level* | `tt_probe`/`search_value` | `score_candidates_with_session` (see row 9 for production-entry behavioral coverage) | `transposition_hit_skips_research` (planner) | Build a small position where two ranked top-level actions transpose to the same child; run `evaluate_after_action` at `depth >= 2` on a fresh `PlannerServices`; `assert!(services.tt_hits > 0)` | **Reach-guard/negative sibling:** a position with **no** transposition (distinct children) yields `tt_hits == 0` — proves the counter tracks real hits, not every node. Revert (remove probe) ⇒ `tt_hits` never increments ⇒ fails. **GAP 3:** the fragile "equality vs a TT-disabled reference within f64 tolerance" assertion is **dropped** — there is no TT-disable seam (adding one is unjustified production surface; no-bool-flags). Its soundness role is carried by row 4 (a *stronger*, precise unit proof) + the ai-gate; this test proves only that the memo fires. |
| 4 | TT never returns a value-divergent aliased entry (**soundness under bounds**) | `tt_probe` exhaustive match | search | `tt_probe_respects_depth_and_bound` (planner, unit) | Insert `TtEntry{depth:1,..}`; assert `tt_probe(key, depth=2, ..)` == `None` (too shallow). Insert `LowerBound value=v`; assert `Some` iff `v>=beta`, else `None`. Symmetric for `UpperBound`/`alpha`. `Exact` always `Some` | Exhaustive over all three `TtBound` arms + the depth-insufficient arm. Revert (return `entry.value` unconditionally) ⇒ shallow/out-of-window asserts fail. **This is the production-relevant soundness guarantee** (a wrong TT hit is impossible by construction), replacing row-3's dropped end-to-end equality. |
| 5a | **Pre-expired deadline collapses to the tactical-only floor** (GAP 4 option-(a) discriminator) | ID loop entry guard | `score_candidates_with_session` (Hard) | `iterative_deepening_pre_expired_deadline_returns_floor` (search) | Interactive config, `search.time_budget_ms = Some(0)` ⇒ `services.deadline` pre-expired at loop entry. Assert every returned score equals `r.score * tactical_weight` (the floor) — i.e. **no** continuation delta and **no** `-1000.0` penalty was applied | **Deterministic** (no wall-clock flake: `after(0)` is reliably expired by loop entry). **Revert-failing for GAP 4:** remove the rung-0 entry guard (option b) ⇒ rung 0 runs ⇒ scores gain a continuation delta ⇒ differ from the floor ⇒ fail. Confirms today's zero-apply collapse is preserved exactly. |
| 5b | **ID's deepest rung reproduces origin/main's fixed-depth pass** (no depth regression; deepest-completed-rung semantics) | ID loop accumulation | `score_candidates_with_session` (Hard) | `iterative_deepening_full_ceiling_matches_fixed_depth` (search) | In **measurement** mode (deadline none ⇒ full ceiling), assert the returned scores equal a reference computed by driving `evaluate_after_action` at `depth = ceiling` for each `ranked` candidate (the origin/main single-pass formula `cont + r.score*tactical_weight`, `-1000.0` on failed sim) | **Deterministic.** **Reach-guard:** assert `ceiling >= 1` for the chosen config (else vacuous). Proves the deepest completed rung == the fixed-depth result the collapse used to skip. Together with 5a (floor) these bracket the ID behavior; the genuinely wall-clock-dependent "which rung completes on a real mid-search expiry" is **not** unit-tested (it would be timing-flaky) — it is covered by the two deterministic endpoints + the accumulation rule (`best_scored` overwritten only when `completed`) + the ai-gate. |
| 6 | Measurement mode is wall-clock-independent (**within-process** byte-determinism) | ID loop deadline gate | measurement path | `measurement_mode_deadline_is_none_and_ceiling_is_pinned` (planner+search) | In measurement assert `services.deadline.expired() == false` and that `score_candidates_with_session` returns **identical** output across two **same-process** runs; assert derived `ceiling == max_depth-1` independent of elapsed time | **Reach-guard:** a `BeamPlusRollout` config with `max_depth=3` ⇒ ceiling must be `2` (non-trivial). Locks the invariant so a future edit reading the clock in measurement fails. **Scope caveat:** assertion is same-process only — cross-process trajectory determinism does NOT hold today (pre-existing engine `HashSet` RandomState, `game_state.rs:2180`; see Determinism contract). A cross-process byte-equality assertion is deliberately NOT written (would flake for reasons unrelated to TT/ID). |
| 7 | WASM axis still deepens | ID ceiling derivation | `create_config(_, Wasm)` | `wasm_ceiling_is_one` (config/search) | For `create_config(Hard, Wasm)` (max_depth capped 2) assert `ceiling == 1` | Sibling: `create_config(Hard, Native)` ⇒ `ceiling == 2`. Proves the platform axis flows through. |
| 8 | No latency regression on pathological boards | whole path (incl. per-node `search_position_hash`) | `choose_action` | Existing `priority_decision_vs_thousand_opponent_tokens_stays_fast` (`search.rs:4091`) — **must still pass** (<100 ms) | Existing assertion unchanged | Pipeline-4's latency baseline gate + this test are the perf guard. The 1000-token worst case stresses the per-object keyword fold; the `is_empty()` guards keep vanilla tokens cheap. If this regresses, the keyword fold is the first suspect. |
| 9 | Determinism regression (ai-duel) + production behavioral coverage | whole path | `score_candidates_with_session` | Existing `score_candidates_with_session_matches_fresh_session` (`:3422`) and measurement sort paths — **must still pass** | Existing | Guards the measurement-sorted output the ID loop now produces; the production-entry check that row-3's dropped equality no longer carries. |

**Coverage-status impact:** none — AI-internal; no card `parse_details`, no coverage
report entry, no Oracle text accepted. (Parser-coverage honesty section is **N/A**:
no parser change.)

---

## AI-gate protocol (behavior-changing pipeline — mandatory) — unchanged

This changes search behavior, so per CLAUDE.md ("AI behavior changes must run
`cargo ai-gate` … paired-seed report attached") and memory
`feedback_preexisting_failure_benchmark_origin_main`:

1. **Baseline binary** — build from an **isolated `origin/main` worktree** (never
   the shared checkout; memory `no-mutate-main`/`never-stash`). Card DB is
   gitignored: pass `PHASE_CARDS_PATH=<card-data.json>` to both baseline and
   candidate runs.
2. **Paired seeds** — 3 seeds, both sides run the **same seed + same game count**,
   measurement mode (`into_measurement`) so the only variable is the binary.
3. **Quick paired gate (mandatory, pre-commit)** — the reduced-game paired
   `cargo ai-gate` across the 3 seeds. **Acceptance:** aggregate candidate win-rate
   **must not regress** vs baseline (may improve); no determinism failure (paired
   seeds reproduce). Blocking gate.
4. **Full-strength tier (dispositioned)** — the full 30-games × 3-seed run
   (~3.5 h). Run **out of band** (not blocking the commit); attach the paired-seed
   report to the PR; if the quick gate is green and the full run later regresses,
   revert. Rationale: TT+ID is expected win-rate-neutral-to-positive (deepest rung
   keeps full budget; TT is sound by row 4), so the quick gate is a sufficient
   pre-commit signal.
5. **No baseline refresh** without the paired-seed report.

## Tilt-first verification cadence

- `cargo fmt --all` (always direct — Tilt doesn't format).
- Then read `tilt logs clippy`, `tilt logs test-ai`, `tilt logs wasm` (WASM axis —
  ID must compile under `Platform::Wasm`; `web_time` handles the deadline there).
  Use `./scripts/tilt-wait.sh clippy test-ai wasm`. Do **not** run
  `cargo build/clippy/test` directly (target-lock contention). Diagnose only after
  `updateStatus == "error"` with `currentBuild.spanID == "none"`.
- `test-engine`/`card-data`/frontend resources are untouched (no engine/card/TS
  edits) — sanity glance only.

## Stop-and-return items (out-of-scope edits I must NOT make)
- **No `crates/engine/src/game/perf_counters.rs` edit** (contended). The TT-hit
  witness is the local `PlannerServices.tt_hits` field, not an engine perf counter.
  If a reviewer insists on an engine-level counter, **stop and return** to the lead.
- **No `duel_suite/**`, `.github/workflows/**`, `policies/**`,
  `effect_classify.rs`, `engine-wasm/**`** edits (concurrent pipelines 4/7).
- **No `Hash` derive on `Keyword` or `CommanderDamageEntry`** (shared engine
  types, `keywords.rs`/`game_state.rs`). The plan folds them locally via serde. If
  a reviewer insists on a derive, that is a shared-type change touching a hot file
  under concurrent edit — **stop and return** to the lead rather than editing it.
- **No `crates/engine/src/util/deadline.rs` edit** (engine util, out of scope; the
  ID tests use `time_budget_ms = Some(0)` for deterministic pre-expiry rather than a
  mock-deadline seam).

## Files touched (final)
- `crates/phase-ai/src/planner/mod.rs` — `search_position_hash`,
  `transposition_key`, `TtBound`, `TtEntry`, TT field + `tt_hits`, `tt_probe`,
  `tt_store`, `search_value` wiring, tests (rows 1–4, 6).
- `crates/phase-ai/src/search.rs` — ID loop in `score_candidates_with_session`,
  imports, tests (rows 5a, 5b, 6, 7).
- `crates/phase-ai/src/config.rs` — doc-comment only (optional).
