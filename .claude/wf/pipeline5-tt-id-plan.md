# Pipeline 5 — Transposition Table + Iterative Deepening in phase-ai Search

## Goal restatement

The policy-guided beam alpha-beta search in `phase-ai` commits to a **fixed** depth
up front and, when the 1500 ms wall-clock deadline fires mid-search, **collapses
every not-yet-searched candidate to a tactical-only score** (`r.score *
tactical_weight`, `search.rs:1702-1705`). It also re-searches identical positions
from scratch (siblings that transpose), because the only memo is a 256-entry
**value-only** eval cache (`planner/mod.rs:509-519`) with no depth/bound semantics.

This pipeline delivers:
- **(a) A transposition table (TT)** — memoizes interior search-node *values with
  alpha-beta bound + depth semantics*, keyed on a position hash, scoped to a single
  decision. On a sufficient-depth hit it returns immediately, skipping the whole
  subtree re-search.
- **(b) Iterative deepening (ID)** — depth `0 → 1 → … → max` under the existing
  deadline, returning the **deepest fully-completed iteration's** scores instead of
  the tactical-only collapse. Measurement mode pins the iteration ceiling (never
  consults the wall clock) so byte-determinism holds.

All work is confined to `crates/phase-ai/src/{planner/mod.rs, search.rs, config.rs}`.
Combat AI (`search.rs ~1502-1515`, a greedy bypass) is untouched.

---

## Analogous Trace (hard gate)

The feature most similar to what we are building is the **existing eval cache +
shared `Deadline` bail path**, traced end-to-end:

- **Config source of the deadline** — `config.rs:20` `AI_SEARCH_TIME_BUDGET_MS =
  Some(1500)`; consumed at `config.rs:110` (`SearchConfig.time_budget_ms`).
- **Deadline construction** — `planner/mod.rs:364-370` (`PlannerServices::new`)
  gates on `execution_mode.is_measurement()`: measurement ⇒ `Deadline::none()`,
  interactive ⇒ `Deadline::after(ms)`. Mirrored onto `AiContext` (`:375-376`).
- **Node/time budget primitive** — `SearchBudget` (`planner/mod.rs:29-71`):
  `exhausted()` = `nodes_evaluated >= max_nodes || deadline.expired()`.
- **Value memo** — `eval_cache: HashMap<u64,f64>` (`planner/mod.rs:331`), keyed by
  bare `quick_state_hash` (`:509-519`), 256-entry cap, value-only.
- **The search itself** — `BeamContinuationPlanner::search_value`
  (`planner/mod.rs:848-925`): alpha-beta minimax at fixed `self.depth`, ordered by
  `rank_candidates` (`:963-985`), leaf = `rollout_estimate` (`:795-830`) →
  `quiesced_leaf_eval` → `evaluate_state_cached`.
- **Search entry** — `score_candidates_with_session` (`search.rs:1483-1740`):
  builds `SearchBudget` (`:1626-1635`), one `build_continuation_planner`
  (`:1637`, depth = `max_depth-1`), ranks top-level candidates (`:1673-1692`),
  walks them (`:1699-1719`) calling `planner.evaluate_after_action` — **this is
  the exact loop ID restructures**, and `:1702-1705` is the collapse we remove.
- **The `Deadline` type** — `engine/src/util/deadline.rs`: `after(ms)`, `none()`
  (const, never expires), `expired()`, `remaining()`. `web_time::Instant` ⇒ works
  native + WASM.

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
document is CLAUDE.md's design principles (building blocks, parameterize-don't-
proliferate, no bool flags, exhaustive match) plus the AI-gate protocol. Confirmed
no skill checklist applies; none is skipped.

---

## Architectural sections

### Pattern Coverage
This is not card-scoped — it changes the **search algorithm** used for *every*
non-combat, non-deterministic AI decision at `search.enabled` difficulties
(Medium, Hard, VeryHard, CEDH, plus their WASM/multiplayer-scaled variants). It
affects 100% of beam-searched decisions across all decks/formats. There is no
"class of cards" axis; the coverage axis is **every search-enabled decision**, so
the build-for-the-class principle is satisfied at the algorithm level (one TT +
one ID loop serve all positions, no position-specific special-casing).

### Building Blocks (compose, don't reinvent)
- `quick_state_hash` (`planner/mod.rs:95`) — reused as the **base** of the new
  `search_position_hash` (extended, see Identity/Provenance).
- `candidate_cache_key`'s composition idiom (`:228-233`) — the TT key mirrors it:
  `search_position_hash + hash_waiting_for`, so acting-player/decision-type is in
  the key (a maximizing node and a minimizing node at the "same board" never
  alias).
- `engine::util::Deadline` — the ID stop signal; **no new time primitive**.
- `SearchBudget` (`:29-71`) — reused per iteration (`new(max_nodes)` reset each
  rung); its `nodes_evaluated` counter doubles as a re-search-skip witness for the
  TT-hit test.
- `BeamContinuationPlanner` + `evaluate_after_action` (`:928-948`) — reused
  verbatim as the per-iteration searcher; ID just varies its `depth` field.
- `eval_cache` (`:509-519`) — **kept** (leaf static-eval memo); the TT is a
  *separate* interior-node memo (different semantics), not a replacement.
- **One new helper justified**: `search_position_hash(state)` — a strict superset
  of `quick_state_hash` adding the fields it deliberately omits that a *bound-
  returning* TT cannot tolerate aliasing on (per-object keywords, library order).
  Justification in Identity/Provenance below.

### Logic Placement
- `search_position_hash`, `TtEntry`, `TtBound`, the TT field + probe/store
  methods, and the TT integration inside `search_value` → **`planner/mod.rs`**
  (the search engine owns search memoization, exactly where `eval_cache` and
  `search_value` already live).
- The ID loop → **`search.rs` `score_candidates_with_session`** (the decision
  entry that already owns the budget construction, the candidate ranking, and the
  now-removed collapse). ID is an *orchestration* concern over
  `evaluate_after_action`, so it belongs at the call site, not inside the planner.
- No `config.rs` behavior change is required. **Optional (recommended)**: a small
  doc-comment update at `config.rs:9-20` noting the deadline now bounds ID rungs.
  No new config field — ID depth ceiling is derived from the existing
  `max_depth`/`planner_mode`, satisfying parameterize-don't-proliferate.

### Rust Idioms
- `TtBound` is a **typed enum** `{ Exact, LowerBound, UpperBound }` — never a
  bool pair (no-bool-flags rule). Matched **exhaustively** in the probe.
- `TtEntry { depth: u32, value: f64, bound: TtBound }` is `Copy` (no clone cost,
  no stored `CandidateAction`).
- Iteration ceiling derived by exhaustive `match config.search.planner_mode`
  (`BeamOnly => 0`, `BeamPlusRollout => max_depth.saturating_sub(1)`), mirroring
  the existing `build_continuation_planner` match (`:950-961`) — no wildcard.
- `saturating_sub` for depth arithmetic (mirrors existing `:957`).
- The TT probe returns `Option<f64>` (`Some` = cutoff hit) — idiomatic early-out,
  no sentinel values.

### Nom Compliance
**N/A — justified.** No file under `crates/engine/src/parser/` is touched. This is
an AI search-engine change with zero Oracle-text parsing.

### CR Annotations
**N/A — justified.** Per CLAUDE.md and the task brief, AI-layer heuristics
(search depth scheduling, memoization) implement no Comprehensive Rules behavior —
they are decision *quality* optimizations over an already-rules-correct
`apply_as_current_for_simulation`. No `// CR` annotations are added. (The existing
CR comments in `search.rs`, e.g. mulligan/tribute pre-emptions, are untouched.)

### Extension vs Creation
**Extension.** The TT extends the eval-cache/`search_value` memoization pattern;
ID extends the existing single-pass candidate walk into a depth loop over the same
planner. The only genuinely new types are `TtEntry`/`TtBound` (a phase-ai-local
value type — *not* an engine enum in the `/add-engine-variant` gate list, so that
gate does not apply) and the `search_position_hash` helper. Both are the minimum
required and both compose from existing primitives.

### Variant Discoverability
No new variant is added to any engine type enumerated by `/add-engine-variant`
(`QuantityRef`, `Effect`, `TargetFilter`, `Keyword`, …). `TtBound` lives in
`phase-ai`, not `engine`, and models an internal alpha-beta bound classification
(textbook TT), not an MTG game concept. `cargo engine-inventory` is therefore not
consulted (it inventories the engine surface, which is unchanged). Stated
explicitly so the reviewer can confirm the gate is correctly skipped.

### Identity / Provenance Contract — TT hash correctness (the critical section)

**The concern (from the audit):** a wrong TT hit changes search *results* (an
interior node returns a cached value and its whole subtree is skipped), whereas a
wrong eval-cache hit only perturbs one leaf's static score. So the TT key must not
*systematically* alias two positions that the search would value differently.

**`quick_state_hash`'s known omissions** (`planner/mod.rs:133-152`, and the
pipeline-3 note): library is hashed by **length not order**; **per-object
keywords** are not hashed. Everything else search-relevant *is* hashed
(per-object power/toughness/tapped/damage/counters/controller, transient
continuous effects by source id, battlefield composition, hand/graveyard by
ObjectId, stack, restrictions, day/night, monarch, command zone).

**Assessment (evidence-based):**
- *Per-object keywords* — **correctness-critical.** `evaluate_state` weighs
  evasion/threat keywords (flying, etc.), so two positions differing only by a
  granted keyword (e.g. a pumped creature with vs. without flying UEOT) can have
  different eval. A bound-returning TT hit that aliases them corrupts the value.
  → **Must be in the TT key.**
- *Library order* — the eval and the candidate set do **not** read library order
  (candidates come from hand/graveyard, hashed by id; eval ignores order). Aliasing
  on library order is value-neutral, but a draw inside the search horizon (depth
  2–3) can diverge two lines. Cheap to include; included for completeness.

**Decision: introduce a stronger, dedicated `search_position_hash(state)` for the
TT key** (the task's "stronger hash for TT keys" mitigation, chosen over a
verification payload because a single superset hash is more idiomatic and avoids a
second comparison field). It is `quick_state_hash`'s body **plus**:
1. for each `obj_id` in `state.battlefield`, hash `obj.keywords` (the granted +
   printed keyword set — a `Vec<Keyword>`; hash in slice order, which is
   deterministic under the engine's apply model);
2. for each player, hash `player.library` ObjectIds in order (not just length).

This is a **strict superset** of `quick_state_hash`'s fields, so it has **no
systematic omission** relevant to a search value. Residual risk is a random 64-bit
`DefaultHasher` (SipHash) collision within a single decision's few-hundred-entry
table — probability ~1e-16, the same tolerance every production alpha-beta engine
accepts for Zobrist keys. Cost: one extra O(board + library + keywords) pass per
searched node — a few thousand extra hash ops per decision, negligible against the
`apply()` clone-and-simulate that dominates each node.

**Binding / lifetime contract of a TT entry:**
- **Authority type & id:** key = `u64` (`search_position_hash + hash_waiting_for`);
  value = `TtEntry { depth, value, bound }`.
- **Binding time:** written when `search_value` returns a *non-truncated* result
  (see "budget-truncation guard" in the step-by-step). Read at `search_value`
  entry.
- **Live vs snapshot:** the stored `value` is a **snapshot** of the minimax value
  at the stored `depth` under the bound. It is only *reused* when
  `entry.depth >= remaining_depth` and the bound proves an alpha-beta cutoff for
  the current `(alpha, beta)` window — otherwise it is ignored (never blindly
  returned), which is what keeps it sound across ID rungs with different windows.
- **Storage:** `transposition_table: HashMap<u64, TtEntry>` on `PlannerServices`.
- **Consuming fn:** `PlannerServices::tt_probe` / `tt_store`, called only from
  `BeamContinuationPlanner::search_value`.
- **Invalidation / expiration:** **per-decision.** `PlannerServices` is
  constructed fresh in every `score_candidates_with_session` call
  (`search.rs:1517`), so the table is empty at the start of each decision and
  dropped at its end — **no cross-turn staleness is possible.** (Cross-decision
  reuse deliberately deferred — see below.)
- **Multi-authority hostile fixture (proves the binding):** two positions that are
  board-identical *except* one has a creature with a granted keyword (flying).
  `quick_state_hash` returns **equal** for them; `search_position_hash` returns
  **distinct**. Test asserts the distinction (revert-failing: drop the keyword
  hashing and the two hashes collide → test fails). This is the fixture that
  proves the TT will not alias a value-divergent pair.

**Cross-decision / turn-scoped TT — dispositioned:** *deferred, not built.* The
audit asks it as an open question; the answer is **within-decision only for now.**
Rationale: (1) ID's re-search overhead is *entirely intra-decision* (each rung
re-walks the same tree), so a per-decision TT captures ~all of the ID win; (2)
between decisions the state has advanced by one applied action, so cross-decision
hit rate is low while staleness risk (a stored best line no longer legal) is real;
(3) it would require threading turn-scoping + invalidation into `AiSession` (the
`projection_cache` pattern at `session.rs:46,167-199` is the template) for marginal
benefit. If future profiling shows high cross-decision transposition, promote the
table onto `AiSession` with a `turn_number + active_player`-scoped key exactly like
`ProjectionKey`. Noted as a follow-up, not in scope.

---

## Determinism contract (hard constraint — measurement mode)

`ExecutionMode::Measurement` must be a pure function of `(binary, config, seed)` —
`cargo ai-gate` pairing and pipeline 4's new latency baseline gate depend on it.
ID introduces wall-clock-dependent depth **only in interactive mode**. The design
keeps measurement deterministic **structurally**:

- `PlannerServices::new` already sets `deadline = Deadline::none()` when
  `is_measurement()` (`planner/mod.rs:364-370`). `Deadline::none().expired()` is
  **always false**.
- The ID loop's only wall-clock consultation is `services.deadline.expired()`.
  In measurement that is a constant `false`, so the loop **always runs the full
  fixed ceiling** `0..=iteration_ceiling(config)` and returns the deepest rung.
- The per-rung `SearchBudget` in measurement is `SearchBudget::new(max_nodes)`
  (no time limit, `search.rs:1634` path preserved), so `budget.exhausted()` is a
  pure function of `nodes_evaluated` — deterministic.
- **Therefore in measurement, depth is pinned by `config` alone; the clock is
  never read.** Byte-determinism holds. This is asserted by a dedicated test
  (`measurement_mode_deadline_is_none_and_ceiling_is_pinned`).

Interactive mode is the only place `expired()` returns `true`, and there it only
ever *stops early and returns an already-completed rung* — it never feeds the clock
into a stored value or a score.

---

## Step-by-step implementation

### Step 1 — `planner/mod.rs`: add `search_position_hash`
Directly after `quick_state_hash` (ends `:215`), add:

```rust
/// Stronger position hash for the transposition table. Superset of
/// `quick_state_hash`: additionally folds per-object keywords and full library
/// ordering — the two fields `quick_state_hash` omits that a *bound-returning*
/// TT cannot tolerate aliasing on (a wrong TT hit skips a whole subtree, unlike
/// a wrong eval-cache hit which only perturbs one leaf). See the TT design notes.
pub fn search_position_hash(state: &GameState) -> u64 {
    let mut hasher = DefaultHasher::new();
    // Reuse the existing digest as the base, then extend.
    quick_state_hash(state).hash(&mut hasher);
    for &obj_id in &state.battlefield {
        if let Some(obj) = state.objects.get(&obj_id) {
            obj.keywords.len().hash(&mut hasher);
            for kw in &obj.keywords {
                kw.hash(&mut hasher);
            }
        }
    }
    for player in &state.players {
        for &id in &player.library {
            id.hash(&mut hasher);
        }
    }
    hasher.finish()
}
```

Verify at implementation time that `Keyword: Hash` (it is used in `HashSet`/match
elsewhere; if any variant is not `Hash`, hash a stable discriminant/`Debug`
projection instead — **stop-and-return** if a `Keyword` payload is non-hashable).
`player.library` is `Vec<ObjectId>` and `ObjectId: Hash` (already hashed in
`quick_state_hash`).

### Step 2 — `planner/mod.rs`: add TT types + key helper
Near `candidate_cache_key` (`:228`), add the TT key (composes the stronger hash
with `hash_waiting_for`, reusing the existing `hash_waiting_for` fn `:235-238`):

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

### Step 3 — `planner/mod.rs`: add TT state + instrumentation to `PlannerServices`
Add fields after `eval_cache` (`:331`):

```rust
    /// Interior-node search-value memo with alpha-beta bound + depth semantics
    /// (distinct from `eval_cache`, which is a value-only *leaf* memo). Scope is
    /// the `PlannerServices` lifetime — one decision — so no cross-turn staleness.
    transposition_table: HashMap<u64, TtEntry>,
    /// Count of TT cutoffs served this decision. Not an engine perf counter
    /// (perf_counters.rs is out of scope); a local witness that a re-search was
    /// actually skipped, used by the TT-hit regression test.
    pub tt_hits: u32,
```

Initialize both in `PlannerServices::new` (`:377-386`): `transposition_table:
HashMap::new(), tt_hits: 0,`.

Add a cap constant beside the code (mirroring the eval-cache 256 guard idiom):
`const TT_CAPACITY: usize = 4096;` (a decision searches <= `max_nodes` (<=96) x
rungs interior nodes, so this never binds in practice; it is a defensive ceiling).

### Step 4 — `planner/mod.rs`: TT probe/store methods on `PlannerServices`
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
/// replacement: only overwrite when the new entry is at least as deep.
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

### Step 5 — `planner/mod.rs`: wire the TT into `search_value` (`:848-925`)
Restructure the head of `search_value` so the probe happens *before* the expensive
`build_decision_context` + expansion, and the store happens on return, with a
**budget-truncation guard** (never cache a value produced by an exhausted-budget
bail, which is a heuristic, not a true depth-`d` minimax value):

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

    // ... unchanged expansion: build_decision_context, rank, alpha-beta loop ...

    let best = /* existing result */;
    let result = if best.is_infinite() {
        services.evaluate_state_quiesced(state)
    } else {
        best
    };
    // Budget-truncation guard: only memoize a fully-explored node.
    if !budget.exhausted() {
        services.tt_store(key, depth, result, alpha_orig, beta);
    }
    result
}
```

Notes: `depth == 0` leaves stay `rollout_estimate` (sampling-based, path-nudged —
not a clean position value, correctly excluded from the TT). The existing
`services.deadline.expired()` mid-loop break (`:898-900`) is retained; a node that
breaks early on the deadline still falls through to the `!budget.exhausted()` store
guard — but note `budget.exhausted()` includes `deadline.expired()`
(`planner/mod.rs:65`), so a deadline-broken node is **not** stored. Good: only
genuinely completed nodes enter the TT.

### Step 6 — `search.rs`: replace the fixed-depth walk with the ID loop
In `score_candidates_with_session`, inside `if config.search.enabled {` (`:1622`),
after `ranked.truncate(branching)` (`:1692`) **replace** the current walk
(`:1699-1724`, the loop with the `deadline_hit`/tactical-collapse) with:

```rust
// Iterative deepening: rung 0 (quiesced eval) → ceiling. Return the deepest
// *fully completed* rung. The deadline only ever stops us early and hands back an
// already-completed rung — it never collapses to tactical-only unless not even
// rung 0 completes.
let ceiling: u32 = match config.search.planner_mode {
    PlannerMode::BeamOnly => 0,
    PlannerMode::BeamPlusRollout => config.search.max_depth.saturating_sub(1),
};

// Floor / no-regression baseline == today's collapse: tactical-only for every
// candidate. Overwritten by each completed rung; returned as-is only if not even
// rung 0 finishes (identical to origin/main's behavior in that case).
let mut best_scored: Vec<(GameAction, f64)> = ranked
    .iter()
    .map(|r| (r.candidate.action.clone(), r.score * tactical_weight))
    .collect();

for iter_depth in 0..=ceiling {
    // Interactive: stop before a rung we cannot afford. Measurement: deadline is
    // none() ⇒ never triggers ⇒ full fixed ceiling ⇒ deterministic.
    if iter_depth > 0 && services.deadline.expired() {
        break;
    }
    // Fresh node budget per rung: the deepest rung gets the full max_nodes just
    // like origin/main's single pass; shallow rungs are near-free and only warm
    // the shared TT + eval_cache. (See "node cap" note below.)
    let mut budget = match (
        config.execution_mode.is_measurement(),
        config.search.time_budget_ms,
    ) {
        (false, Some(ms)) => SearchBudget::with_deadline(config.search.max_nodes, services.deadline),
        _ => SearchBudget::new(config.search.max_nodes),
    };
    let mut planner = BeamContinuationPlanner {
        depth: iter_depth,
        rollout_depth: config.search.rollout_depth,
    };

    let mut rung_scored = Vec::with_capacity(ranked.len());
    let mut completed = true;
    for r in &ranked {
        if iter_depth > 0 && services.deadline.expired() {
            completed = false; // partial rung — discard, keep previous best
            break;
        }
        let score = if let Some(sim) = apply_candidate(state, &r.candidate) {
            let cont = planner.evaluate_after_action(&sim, &mut services, &mut budget);
            cont + (r.score * tactical_weight)
        } else {
            r.score - 1000.0 // failed simulation — same penalty as origin/main
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

Remove the now-obsolete `budget` construction at `:1626-1635` (moved inside the
loop) and the `deadline_hit` bookkeeping. Use `SearchBudget::with_deadline`
(`planner/mod.rs:56`) so each rung's budget shares the one `services.deadline`
(the interior `budget.exhausted()`/mid-loop bail still fire on time). Keep
`is_target_selection`/`is_stack_response`/`tactical_weight` (`:1645-1666`)
unchanged. `BeamContinuationPlanner` and `PlannerMode` are already imported via
`crate::planner::…` / `crate::config::…`; add `BeamContinuationPlanner` and
`transposition_key`/`search_position_hash` to the `use crate::planner::{…}` list
(`search.rs:28-30`) as needed (or reference fully-qualified).

**Node-cap disposition (explicit, for the reviewer):** each rung resets the node
budget to `max_nodes`, so the *deepest* rung (the origin/main-equivalent) gets the
**full** `max_nodes` — it is never starved, guaranteeing no depth regression at the
target depth; the TT only *accelerates* it. Total `apply()` across rungs is bounded
by `(ceiling+1) x max_nodes`, but rung 0 does zero `search_value` ticks
(`evaluate_after_action` at depth 0 returns `evaluate_state_quiesced` directly) and
rung 1 is tiny, so the practical overhead is a few extra evals, dwarfed by the
deepest rung. The alternative (one shared budget across all rungs) was **rejected**:
it would let shallow rungs consume the cap and starve the deepest rung below
origin/main depth — a direct win-rate-regression risk. This reading treats
`max_nodes` as it has always been defined: the ceiling of a single depth-committed
search pass.

### Step 7 — `config.rs` (optional doc-only)
Update the module doc at `config.rs:9-16` to note the wall-clock budget now bounds
*iterative-deepening rungs* (deepest completed rung is returned on expiry) rather
than a single fixed-depth pass. No code/field change.

---

## Verification Matrix

Runtime tests colocated in `#[cfg(test)]` in `planner/mod.rs` and `search.rs`.
Run via `cargo test -p phase-ai` semantics (Tilt `test-ai` resource). Each row
names the seam, the revert-failing assertion, and its paired reach-guard.

| # | Claim | Changed seam | Production entry | Test (add) | Revert-failing assertion | Reach-guard / sibling / hostile |
|---|-------|-------------|------------------|-----------|--------------------------|-------------------------------|
| 1 | Stronger hash distinguishes a keyword-only difference that `quick_state_hash` aliases | `search_position_hash` | TT key | `search_position_hash_distinguishes_granted_keyword` (planner) | `assert_ne!(search_position_hash(a), search_position_hash(b))` for two states differing only by one battlefield object's `keywords` (a has flying) | **Reach-guard:** first `assert_eq!(quick_state_hash(a), quick_state_hash(b))` — proves the base hash *does* alias, so the `_ne!` is non-vacuous. Revert (drop keyword folding) ⇒ both equal ⇒ fails. |
| 2 | Stronger hash distinguishes library ordering | `search_position_hash` | TT key | `search_position_hash_distinguishes_library_order` | `assert_ne!` for two states with swapped top-two library ObjectIds | **Reach-guard:** `assert_eq!(quick_state_hash(a), quick_state_hash(b))` (base hashes length only). Sibling to row 1. |
| 3 | A TT hit skips a real re-search (the core memoization win) | `tt_probe`/`search_value` | `score_candidates_with_session` search | `transposition_hit_skips_research` (search) | Build a small position where two ranked top-level actions transpose to the same child; run search; `assert!(services.tt_hits > 0)` **and** `assert_eq!` scored result vs a from-scratch (TT-disabled) reference within f64 tolerance | **Reach-guard/negative:** a *sibling* position with **no** transposition (distinct children) yields `tt_hits == 0` — proves the counter tracks real hits, not every node. Revert (remove probe) ⇒ `tt_hits` never increments ⇒ fails. Uses `SearchBudget.nodes_evaluated`/`tt_hits` as the witness (no engine perf_counter). |
| 4 | TT never returns a value-divergent aliased entry (soundness under bounds) | `tt_probe` exhaustive match | search | `tt_probe_respects_depth_and_bound` (planner, unit) | Insert `TtEntry{depth:1,..}`; assert `tt_probe(key, depth=2, ..)` returns `None` (too shallow); insert `LowerBound value=v`; assert returns `Some` only when `v>=beta`, `None` when `v<beta`; symmetric for `UpperBound`/`alpha` | Exhaustive over all three `TtBound` arms + the depth-insufficient arm. Revert (return `entry.value` unconditionally) ⇒ shallow/out-of-window asserts fail. |
| 5 | ID returns the deepest **completed** rung, not tactical-only, on deadline expiry | ID loop in `score_candidates_with_session` | `choose_action` (Hard) | `iterative_deepening_returns_completed_rung_not_collapse` (search) | With a *tiny but non-zero* interactive deadline that lets rung 0/1 finish but not rung 2, assert the returned scores equal the rung-1 result (not `r.score*tactical_weight` for all) — compare against a reference rung-1-only run | **Reach-guard:** assert the same state under `into_measurement` (no deadline) returns the *deeper* rung-2 result, proving rungs differ (else the assertion is vacuous). **Hostile:** a deadline so tiny even rung 0 can't complete ⇒ falls back to the tactical-only baseline == origin/main (no worse). |
| 6 | Measurement mode is wall-clock-independent (byte-determinism) | ID loop deadline gate | measurement path | `measurement_mode_deadline_is_none_and_ceiling_is_pinned` (planner+search) | In measurement mode assert `services.deadline.expired() == false` and that `score_candidates_with_session` returns **identical** output across two runs; assert the derived `ceiling` equals `max_depth-1` independent of any elapsed time | **Reach-guard:** a `BeamPlusRollout` config with `max_depth=3` ⇒ ceiling must be `2` (non-trivial). Revert (consult clock in measurement) — not possible since deadline is none(); test locks the invariant so a future edit that reads the clock in measurement fails. |
| 7 | WASM axis still deepens | ID ceiling derivation | `create_config(_, Wasm)` | `wasm_ceiling_is_one` (config/search) | For `create_config(Hard, Wasm)` (max_depth capped 2) assert `ceiling == 1` (rungs 0→1 run) | Sibling: `create_config(Hard, Native)` ⇒ `ceiling == 2`. Proves the platform axis flows through. |
| 8 | No latency regression on pathological boards | whole path | `choose_action` | Existing `priority_decision_vs_thousand_opponent_tokens_stays_fast` (`search.rs:4091`) — **must still pass** (<100ms) | Existing assertion unchanged | Pipeline-4's latency baseline gate + this test are the perf guard; ID's shallow rungs must not blow the ceiling. |
| 9 | Determinism regression (ai-duel) | whole path | `score_candidates_with_session` | Existing `score_candidates_with_session_matches_fresh_session` (`:3422`) and measurement sort paths — **must still pass** | Existing | Guards the measurement-sorted output ordering the ID loop now produces. |

**Coverage-status impact:** none — this is AI-internal; no card `parse_details`,
no coverage report entry, no Oracle text accepted. (Parser-coverage honesty section
is **N/A**: no parser change, no Oracle text newly accepted/deferred.)

---

## AI-gate protocol (behavior-changing pipeline — mandatory)

This changes search behavior, so per CLAUDE.md ("AI behavior changes must run
`cargo ai-gate` … paired-seed report attached") and the memory note
`feedback_preexisting_failure_benchmark_origin_main`:

1. **Baseline binary** — build the AI-gate baseline from an **isolated
   `origin/main` worktree** (never the shared checkout; memory
   `no-mutate-main`/`never-stash`). The card DB is gitignored: pass
   `PHASE_CARDS_PATH=<path to card-data.json>` to both baseline and candidate runs.
2. **Paired seeds** — 3 seeds, both sides (baseline vs candidate) run the **same
   seed + same game count**, measurement mode (`into_measurement`) so results are
   deterministic and the only variable is the binary.
3. **Quick paired gate (mandatory, pre-commit)** — the reduced-game paired
   `cargo ai-gate` run across the 3 seeds. **Acceptance:** aggregate candidate
   win-rate **must not regress** vs baseline (may improve); no determinism failure
   (paired seeds must reproduce). This is the blocking gate.
4. **Full-strength tier (explicitly dispositioned)** — the full
   30-games x 3-seed run (~3.5h). **Disposition:** run it *out of band* (not
   blocking the commit) and attach the paired-seed report to the PR; if the quick
   gate is green and the full run later shows a regression, revert. Rationale: the
   TT+ID change is expected to be win-rate-neutral-to-positive (deepest rung keeps
   full budget), so the quick gate is a sufficient pre-commit signal and the full
   run is confirmatory.
5. **No baseline refresh** without the paired-seed report (CLAUDE.md: "refresh
   baselines only with the paired-seed report attached").

## Tilt-first verification cadence

- `cargo fmt --all` (always direct — Tilt doesn't format).
- Then read `tilt logs clippy`, `tilt logs test-ai`, `tilt logs wasm` (WASM axis —
  ID must compile under `Platform::Wasm`; `web_time` already handles the deadline
  there). Use `./scripts/tilt-wait.sh clippy test-ai wasm`. Do **not** run
  `cargo build/clippy/test` directly (target-lock contention). Diagnose only after
  `updateStatus == "error"` with `currentBuild.spanID == "none"`.
- `test-engine`/`card-data`/frontend resources are **not** touched by this change
  (no engine, card, or TS edits) — no need to gate on them beyond a sanity glance.

## Stop-and-return items (out-of-scope edits I must NOT make)
- **No `crates/engine/src/game/perf_counters.rs` edit** (contended by another
  pipeline). The TT-hit witness is the local `PlannerServices.tt_hits` field, *not*
  an engine perf counter. If a reviewer insists on an engine-level counter, **stop
  and return** to the lead rather than editing `perf_counters.rs`.
- **No `duel_suite/**`, `.github/workflows/**`, `policies/**`,
  `effect_classify.rs`, `engine-wasm/**`** edits (concurrent pipelines 4/7).
- If `Keyword` (or a payload) turns out not to be `Hash`, **stop and return** with
  the specific variant rather than inventing a bespoke projection.

## Files touched (final)
- `crates/phase-ai/src/planner/mod.rs` — `search_position_hash`,
  `transposition_key`, `TtBound`, `TtEntry`, TT field + `tt_hits`, `tt_probe`,
  `tt_store`, `search_value` wiring, tests.
- `crates/phase-ai/src/search.rs` — ID loop in `score_candidates_with_session`,
  imports, tests.
- `crates/phase-ai/src/config.rs` — doc-comment only (optional).
