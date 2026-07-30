# Pipeline 6 — Determinized Opponent Sampling on `deck_knowledge`

**Task:** Replace the AI planning path's perfect-information cheat (it sees the opponent's real hand + real library order during simulation) with **determinized sampling**: construct K plausible opponent hidden states consistent with public information + the sanctioned `deck_knowledge` model + engine reveal sets, run the existing search against each, and aggregate.

**Scope decision (stated up front):** v1 determinizes **all opponents' hidden zones (hand + library together)** at the *root* of the `score_candidates` spell/priority planning path, and runs the existing (untouched) beam/rollout/quiescence search on each sample. This is a **complete, gate-passing, reviewable unit**. Two paths are explicitly **deferred to named follow-ups** (see §12) because they use *different machinery* (`project_to`, not `score_candidates`) and would bloat this diff:
- **P6-followup-A:** determinize the opponent-turn **projection** input consumed by `combat_ai.rs` crackback (`combat_ai.rs:354-387` → `project_to`) and by `threat_profile.rs:287`.
- **P6-followup-B:** honor engine **DFC back-face** hydration for sampled dual-faced cards (requires threading the `CardDatabase` into the scoring path; front-face cost/abilities are already correct in v1).

> **Why not a smaller "hand-only" slice?** From the AI's perspective the opponent's hand *and* library draw from the **same** unknown pool (`decklist − public zones − revealed`). Sampling "hand only" from `decklist − public − library` reproduces the *real* hand exactly (since real hand = decklist − public − library), hiding nothing. Hand and library must be redistributed **together** from the shared pool. So the coherent minimal slice is the full hidden-zone determinization; the genuine reductions are the two deferred *machinery* paths above.

---

## 0. Applicable skills (engine-planner Step 1)

| Skill | Applies? | Note |
|---|---|---|
| `add-ai-feature-policy` | **Partially — conventions only.** | This is **not** a `DeckFeatures` axis or a `TacticalPolicy`; the feature/policy scaffold does NOT apply. We adopt its non-negotiable phase-ai conventions: grep-verified CR annotations, band helpers where scores are involved (n/a here), the `no_name_matching` lint (n/a — no card-name classification), and the **`cargo ai-gate` paired-seed protocol** (§11). |
| `add-engine-variant` | **Yes if a variant is added.** | We add **no** engine enum variant. We add one plain `usize` config field (`determinization_samples`) to the phase-ai search-config struct — not an engine enum. `cargo engine-inventory` consulted: determinization is search infra, no `Effect`/`QuantityRef`/`TargetFilter`/etc. touched. Gate satisfied by non-applicability. |
| `card-test` | **Yes.** | The discriminating "cheat removed" runtime test (§10, test D) casts through the AI scoring path; follows the GameScenario/GameRunner conventions where a cast is exercised. |

No existing skill checklist governs **AI search infrastructure**; we follow phase-ai conventions + the ai-gate protocol as the runnable gate.

---

## 1. Analogous Trace (hard gate)

**Traced feature:** the existing WASM **root-parallelism ensemble** + the `score_candidates` planning flow, plus the `deck_knowledge → threat_profile` distribution consumer, plus the deck-load object-instantiation chain. Full end-to-end path followed:

1. **Ensemble entry (the shape we mirror):** `client/src/adapter/ai-worker-pool.ts:101-131` (`baseSeed = Date.now()`, worker `i` seeded `baseSeed+i`, `mergeScores` **averages per-action scores** keyed by action identity) → `client/src/adapter/engine-worker.ts:305-318` → `crates/engine-wasm/src/lib.rs:1292-1311` `get_ai_scored_candidates` (**re-seeds `state.rng = ChaCha20Rng::seed_from_u64(rng_seed)`**) → `crates/phase-ai/src/search.rs:1484` `score_candidates_with_session` → `crates/phase-ai/src/planner/mod.rs:491-514` `PlannerServices::new` (**derives its own `Deadline::after(time_budget_ms)`**) → beam `search_value` / rollout / quiescence, each `state.clone()`-ing the real state (`planner/mod.rs:1175-1179` `apply_candidate`).
2. **Distribution consumer (the model we stay consistent with):** `crates/phase-ai/src/deck_knowledge.rs:60` `remaining_deck_view` → `crates/phase-ai/src/threat_profile.rs:167-199` `build_threat_profile` (already treats the opponent deck as a **hypergeometric distribution**, not perfect info) → `context.rs:26` `AiContext.opponent_threat`.
3. **Object instantiation (the primitive we reuse):** `crates/engine/src/game/deck_loading.rs:620` `load_deck_into_state` → `deck_loading.rs:214-226` `create_object_from_card_face` → `crates/engine/src/game/printed_cards.rs:92-237` `apply_card_face_to_object` (flattens a `CardFace` onto a `GameObject` in place).

The determinizer sits **between** the search entry and the search body: it produces a sampled `GameState`, and the untouched search runs on it. This mirrors how the WASM worker re-seeds `state.rng` before the *same* `score_candidates_with_session` call — we extend that "prepare state, then score" seam.

---

## 2. Pattern Coverage

- **Class, not card:** the determinizer operates on *any* opponent's hidden zones for *any* deck via the decklist (`GameState.deck_pools`) — it is card-agnostic. It covers **100% of AI planning decisions** in 2-player and multiplayer where the opponent has a known decklist. There is no card-specific logic.
- **Estimated impact:** every AI priority/spell-cast decision in every game (the `score_candidates` path). The cheat currently benefits the AI in *every* game with hidden cards; determinization removes it universally.

---

## 3. Building Blocks (compose, don't reinvent)

| Building block | File:line | Use |
|---|---|---|
| `deck_knowledge::accounted_object_ids` | `deck_knowledge.rs:85-125` | **Refactor** into public-vs-hidden split (§6). Currently subtracts hand (a hidden zone) — wrong for opponent-pool sampling. |
| `deck_knowledge::DeckCardKey`, `deck_entry_key`, `object_key` | `deck_knowledge.rs:10-17, 127-141` | Reused verbatim for pool-conservation keying. |
| Decklist source | `deck_knowledge.rs:29` (`state.deck_pools.iter().find(...)`) — `PlayerDeckPool.current_main: Arc<Vec<DeckEntry>>` (ordered) | Ground-truth pool source. `DeckEntry.card` is a full `CardFace` (abilities/cost/types) — no DB needed. |
| `apply_card_face_to_object` | `printed_cards.rs:92-237` | In-place identity overwrite of an existing `GameObject` from a `CardFace`. Sets all castability fields + `base_*` mirrors + `printed_ref`. |
| `printed_ref_from_face` | `printed_cards.rs:64-72` | Not needed directly (apply_card_face sets printed_ref), but confirms identity handle derivation. |
| Engine RNG `ChaCha20Rng` | `game_state.rs:5864-5867`, `default_rng` `:42-44` | Seed source; cross-platform deterministic (native+WASM). |
| Shuffle primitive | `rand::seq::SliceRandom::shuffle` on a `Vec` (as used engine-side, e.g. `ability_utils.rs:1259`) | Seeded shuffle of the ordered pool. |
| `planner::quick_state_hash` | `planner/mod.rs:96-216` | Per-decision seed mixing (varies samples by position). Public within-crate. |
| `Deadline` | `engine/src/util/deadline.rs` (`web_time::Instant`, `Copy`) | Shared wall-clock ceiling across K samples. |
| `score_candidates_with_session` | `search.rs:1484` | Split into ensemble wrapper + core (§7). |

**One new module** is justified: `crates/phase-ai/src/determinize.rs` — the `Determinizer` is a composable primitive (a `&GameState → GameState` sampler), reused by v1's search path and by both follow-ups. It is not a one-off.

---

## 4. Logic Placement

| Piece | Layer | Justification |
|---|---|---|
| Sampling pool computation (`unknown_hidden_pool`) | `phase-ai/deck_knowledge.rs` | Extends the existing sanctioned knowledge model; keeps the "what the AI may know about a deck" logic in one module. |
| Determinizer (state resample) | **new** `phase-ai/determinize.rs` | AI-only simulation concern. **Not** an engine visibility rule — the engine's authoritative `GameState`/`filter_state_for_viewer`/multiplayer `SimulationFilter` are **untouched** (requirement #5). The AI builds its *own* sampled state. |
| Ensemble loop + aggregation + budget split | `phase-ai/search.rs` (`score_candidates_with_session` wrapper) | Sits at the existing scoring seam; all callers (native + WASM) inherit it. |
| `determinization_samples` config field + per-preset values | `phase-ai/config.rs` | Difficulty/platform gating lives with the other search knobs (`max_depth`/`max_nodes`/`time_budget_ms`). |
| Object identity overwrite | reuse `engine::game::printed_cards::apply_card_face_to_object` | Engine owns object construction; we call its primitive, we do not duplicate it. |

No frontend change required — the WASM worker path already calls `score_candidates_with_session`; the ensemble becomes transparent (the frontend's own `mergeScores` still averages across workers on top, which is additive and fine).

---

## 5. Rust Idioms

- **No bool flags.** `determinization_samples: usize` where `0` is the disabled sentinel (matches the existing `rollout_samples`/`max_nodes` numeric-knob convention in `config.rs`) — an integer count, not a bool.
- **Ordered iteration only** for determinism (see §8): collect slots from `player.hand`/`player.library` (`Vec`s) and the pool from `deck_pools.current_main` (ordered `Arc<Vec<DeckEntry>>`); **never** iterate `state.objects` (`HashMap`) where order affects output (guards against the #4878 class of cross-process nondeterminism).
- **Typed identity handles:** reuse `DeckCardKey` (already `Eq + Hash`) for pool conservation; reuse `HashSet<ObjectId>` for the pinned-known set.
- **Exhaustive handling** of the pinned-vs-unknown partition (a card is pinned iff its id ∈ known set), no wildcard fallthrough.
- Determinizer returns an owned `GameState` (search already clones per call), matching `apply_candidate`'s clone-and-mutate idiom.

---

## 6. `deck_knowledge` refactor — the sampling pool

**Problem:** `accounted_object_ids` (`deck_knowledge.rs:85-125`) subtracts the player's **hand** (line 89) — correct for "cards remaining in *my* library" but wrong for sampling an *opponent's* combined hidden pool, because it removes the very cards we need to redistribute.

**Refactor (surgical):**
1. Split `accounted_object_ids` into `public_account_object_ids(state, player)` — graveyard + owned-battlefield + owned-exile + stack-spells (lines 90-122, **excluding** the hand line 89 and never touching library) — and keep `accounted_object_ids` as `public_account_object_ids(...) ++ player.hand` so **existing callers** (`known_remaining_deck_counts`, used by `tutor.rs`/`threat_profile.rs`) are behavior-preserved.
2. Add:

```rust
/// CR 400.2: hand and library are hidden zones. From `viewer`'s perspective an
/// opponent's hidden cards are unknown EXCEPT those the engine has revealed.
/// Returns the ordered multiset (decklist order) of card faces that could occupy
/// `player`'s unknown hidden-zone slots — i.e. decklist − public-zone cards −
/// hidden cards whose identity `viewer` legitimately knows (`known_ids`).
pub fn unknown_hidden_pool(
    state: &GameState,
    player: PlayerId,
    known_ids: &HashSet<ObjectId>,
) -> Vec<CardFace> { /* expand decklist entries in decklist order, decrement per
    public-zone object and per known hidden card, skip tokens/no-key objects */ }
```

Pool is built by expanding `current_main` entries **in decklist order** (deterministic), decrementing a working `HashMap<DeckCardKey,u32>` for each public-zone object (via `object_key`) and each pinned-known hidden object. Order-insensitive counting via HashMap is fine; the **expansion back to a Vec walks the ordered decklist**, so pool order is deterministic before shuffle.

---

## 7. Determinizer + ensemble wiring

### 7a. `crates/phase-ai/src/determinize.rs` (new)

```rust
/// Produce an AI-simulation-only clone of `state` in which every opponent's
/// genuinely-unknown hidden-zone cards are resampled to a plausible assignment
/// consistent with public info + engine reveal sets. The AI player's own zones,
/// all public zones, and all revealed/known cards are left byte-identical.
///
/// CR 400.2 (hand/library are hidden), CR 401.2 (library order is unknown),
/// CR 401.3 (library/hand SIZE is public — preserved exactly), CR 701.20a
/// (revealed cards are known to all players — pinned).
pub fn determinize_opponents(
    state: &GameState,
    ai_player: PlayerId,
    rng: &mut ChaCha20Rng,
) -> GameState {
    let mut sim = state.clone();
    for opponent in opponents_of(&sim, ai_player) {           // ordered PlayerId walk
        // 1. known set: cards whose identity ai_player legitimately knows.
        //    revealed_cards ∪ public_revealed_cards, plus private_look_ids iff
        //    sim.private_look_player == Some(ai_player). (CR 400.2 / 701.20a)
        let known = pinned_known_ids(&sim, ai_player, opponent);
        // 2. ordered unknown slots: iterate player.hand then player.library (Vecs),
        //    skip ids in `known` and skip tokens. Order: hand slots, then library
        //    slots in library order.
        let slots: Vec<ObjectId> = unknown_slots(&sim, opponent, &known);
        // 3. pool of candidate faces (decklist order).
        let mut pool = deck_knowledge::unknown_hidden_pool(&sim, opponent, &known);
        if pool.is_empty() || slots.is_empty() { continue; }   // no decklist / nothing unknown
        // 4. seeded shuffle, then deal faces to slots. Both hand and library slots
        //    draw from ONE shuffled pool (CR 401.2/402: which unknown card is in
        //    hand vs library is itself unknown).
        pool.shuffle(rng);
        for (slot, face) in slots.iter().zip(pool.iter()) {
            if let Some(obj) = sim.objects.get_mut(slot) {
                apply_card_face_to_object(obj, face);          // in-place identity swap
            }
        }
        // slots.len() may exceed pool.len() only on decklist inconsistency
        // (tokens/copies in hidden zones counted as deck cards elsewhere): leftover
        // slots keep their real identity — a rare, bounded residual, logged in debug.
    }
    sim
}
```

Notes:
- **ObjectIds and zone Vecs are never mutated** — only object *identity* fields are overwritten in place. Library positions are preserved as slots; the *identities* assigned to library slots are shuffled, which is exactly the CR 401.2 "order unknown" randomization at the identity level. Determinized objects only ever live in hand/library (hidden zones) → no layer recompute needed.
- **DFC caveat (v1):** `apply_card_face_to_object` does not set `back_face` (needs DB rehydrate). Sampled dual-faced opponent cards have correct front-face cost/abilities; alternate-face casting stays inert → **P6-followup-B**. Documented in the module doc comment.

### 7b. `crates/phase-ai/src/search.rs` — ensemble wrapper

Split `score_candidates_with_session` (`search.rs:1484`):
- Rename the current body → `score_candidates_core(state, ai_player, config, session, deadline_override: Option<Deadline>)`. The **only** internal change: when `deadline_override` is `Some`, pass it into `PlannerServices::new` instead of deriving from `config.time_budget_ms`. Requires a `PlannerServices::with_deadline(...)` constructor (or a `deadline: Option<Deadline>` param) — a minimal, additive change to `planner/mod.rs:491-514`. When `None`, behavior is byte-identical to today.
- New `score_candidates_with_session` wrapper:

```rust
pub fn score_candidates_with_session(state, ai_player, config, session) -> Vec<(GameAction, f64)> {
    let k = config.search.determinization_samples;
    if k == 0 {
        return score_candidates_core(state, ai_player, config, session, None); // unchanged path
    }
    // ONE shared wall-clock ceiling across all K sequential samples.
    let deadline = Deadline::after(config.search.time_budget_ms); // or Deadline::none() in measurement mode, mirroring PlannerServices::new
    let base_seed = quick_state_hash(state)              // varies by position (per-decision)
        .wrapping_add(state.rng_seed);                   // varies by game/worker (WASM re-seeds rng_seed upstream? no — mix rng too)
    let mut acc: Vec<(GameAction, f64)> = Vec::new();    // first-seen order preserved
    let mut counts: HashMap<GameActionKey, usize> = HashMap::new();
    for i in 0..k {
        let mut rng = ChaCha20Rng::seed_from_u64(base_seed.wrapping_add(i as u64 * SPLIT));
        let sampled = determinize_opponents(state, ai_player, &mut rng);
        let scored = score_candidates_core(&sampled, ai_player, config, session, Some(deadline));
        merge_into(&mut acc, &mut counts, scored);       // sum per action
    }
    finalize_mean(acc, counts)                            // divide sums by observed count
}
```

- **Aggregation = mean** per action (matches the frontend `mergeScores` contract). Root candidate set is **identical across samples** (determinization changes only opponent hidden zones, never the AI's own legal actions), so every action is observed in all K samples; mean is well-defined. Key actions by their existing equality (or a canonical serialization, mirroring `mergeScores`' `JSON.stringify`).
- **Seed source:** `quick_state_hash(state) ⊕ state.rng_seed ⊕ splitmix(i)`. Within-process reproducible given the same state+seed (requirement #2). The WASM worker already re-seeds `state.rng` (not `rng_seed`) per worker; to make workers diverge, **also fold `state.rng.clone().next_u64()`** into `base_seed` (clone so `&GameState` stays immutable) — or, simpler, fold the worker seed the frontend already varies. **Decision:** fold `state.rng_seed` **and** a `state.rng.clone().next_u64()` draw so both native (distinct `rng_seed` per game) and WASM (distinct re-seed per worker) produce distinct ensembles. Cross-process byte-identity remains gated by #4878 (acknowledged, out of scope).

### 7c. `crates/phase-ai/src/config.rs` — gating

Add `pub determinization_samples: usize` to the per-difficulty search-config struct (the one holding `max_depth`/`max_nodes`/`max_branching`/`time_budget_ms`). Per-preset values:

| Preset | K | Rationale |
|---|---|---|
| Medium (`config.rs:780`) | **0** | Keep the fast/low tier cheap and unchanged; determinization is a "higher tiers only" feature (requirement #4, task gate). |
| Hard (`:803`) | **2** | 2 samples ≈ halves the single-sample variance at 2× base cost; node cap 48 keeps each search short. |
| VeryHard (`:826`) | **3** | 3 samples materially de-bias without runaway cost; node cap 64. |
| CEDH (`:849`) | **3** | Same as VeryHard; multiplayer + node cap 96 already dominates cost. |
| WASM override (`:885-889`) | **`min(2, tier)`** | The frontend worker pool **already** provides cross-sample root parallelism (`ai-worker-pool.ts` merges N workers). Cap per-worker K at 2 so effective samples = N_workers × K without per-worker latency blow-up. |
| Multiplayer (`:910-945`) | inherit tier, but **1** if `players > 2` beyond a node-cost threshold | Determinizing 3+ opponents per sample multiplies pool work; keep K modest. |

**K justification (requirement #4):** K∈{2,3} is the empirically standard low end for determinized search where per-sample cost is high; it captures most of the variance-reduction benefit (variance ∝ 1/K) before diminishing returns, and keeps the multiplier bounded. **Multiplication happens at the root (ensemble of K full searches), not per-node** — per-node determinization would re-randomize mid-search and destroy the TT/eval-cache consistency that keys on fixed identities within a search.

**Perf budget argument (requirement #4):** the **node cap (24–96) is the primary bound** and is deterministic regardless of time; a single search typically completes well under 1500ms. K sequential searches therefore usually finish in K×(sub-cap time) ≪ K×1500ms. The **shared wall-clock `Deadline`** (created once in the wrapper, threaded into all K via `deadline_override`) caps *aggregate* latency at ~1500ms: early fast samples leave budget for later ones; if a sample is pathologically slow, later samples' iterative deepening returns their deepest-completed rung (or the no-regression tactical floor at `search.rs:1694-1697`) — **never empty, never a crash**. Iterative deepening thus provides graceful degradation exactly as it does today, now shared across the ensemble.

**TT/eval-cache interaction (requirement re: commit 96e50bcf5):** each sample runs in its **own** `PlannerServices` (fresh per `score_candidates_core` call) → its own fresh 4096-entry TT and eval_cache. `search_position_hash` folds full per-player **library ObjectId order** (`planner/mod.rs:262-266`); since determinization keeps object *IDs* in place and only swaps identities, the per-sample hash is internally consistent, and the separate per-sample TTs mean **zero cross-sample contamination** (and the eval_cache `quick_state_hash` aliasing risk the audit flagged cannot occur across samples because caches are not shared). This is correct-by-construction; no TT change needed.

---

## 8. Determinism (requirement #2) — the #4878 discipline

- All ordering-sensitive collection uses **ordered `Vec`s** (`player.hand`, `player.library`, `deck_pools.current_main`), never `state.objects`/`HashMap` iteration. The seeded `ChaCha20Rng` shuffle is applied to a Vec whose pre-shuffle order is decklist order → deterministic within-process.
- The pinned-known set is a `HashSet<ObjectId>` used only for **membership tests** (order-irrelevant), never iterated to produce output order.
- Seeds derive from `quick_state_hash ⊕ rng_seed ⊕ rng draw ⊕ splitmix(i)` — reproducible given identical state+seed in-process.
- **Cross-process** byte-identity is *not* guaranteed (GH #4878: `HashMap`/`HashSet` `RandomState` leaks into AI tie-breaking upstream of us). Acknowledged and out of scope; the ai-gate paired-seed protocol runs same-process so it is unaffected.

---

## 9. Extension vs Creation

- **Extends** the existing "prepare state → score" seam (WASM already re-seeds `state.rng` before scoring), the `deck_knowledge` module (public/hidden split + pool), and reuses `apply_card_face_to_object`.
- **Creates** exactly one new module (`determinize.rs`) — justified as a reusable primitive consumed by v1 and both follow-ups.
- **No engine variant, no new engine type, no engine visibility change** (requirement #5).

---

## 10. Verification Matrix (discriminating runtime tests)

All tests in `determinize.rs` `#[cfg(test)]` unless noted. Runtime tests drive the production `determinize_opponents` / `score_candidates_with_session` entry points (not hand-built shapes).

| # | Test | Seam / production entry | Revert-failing assertion | Positive reach-guard | Hostile / negative sibling |
|---|---|---|---|---|---|
| A1 | `preserves_public_and_own_zones` | `determinize_opponents` | opponent battlefield/graveyard/exile **and** AI's own hand/library are identity-identical after determinize | assert ≥1 opponent hidden card **did** change (else vacuous) | — |
| A2 | `preserves_hidden_zone_sizes` (CR 401.3) | `determinize_opponents` | `opp.hand.len()` and `opp.library.len()` unchanged | as A1 | opponent with 0-card hand / empty library → no panic |
| A3 | `pins_revealed_cards` (CR 400.2/701.20a) | `determinize_opponents` | put an opp hand-card id in `public_revealed_cards`; its identity is **unchanged** after determinize | assert a *different* unknown opp card **did** change (proves resampling ran) | id in `revealed_cards`; id in `private_look_ids` w/ `private_look_player==ai` (pinned) vs `==opponent` (NOT pinned) |
| A4 | `samples_only_from_decklist` (conservation) | `determinize_opponents` | every determinized hidden identity ∈ decklist multiset; (public ∪ hidden) multiset == decklist | — | token in opponent hand → left untouched (no DeckCardKey) |
| A5 | `seed_deterministic_and_position_varying` | wrapper seed path | same (state,seed) → identical sample; two different states → different sample | — | — |
| A6 | `empty_deck_pool_is_noop` | `determinize_opponents` | opponent without `deck_pools` entry → state unchanged | — | pool smaller than unknown slots → leftover slots keep real identity, no panic |
| B1 | `k0_equals_baseline` | `score_candidates_with_session` | with `determinization_samples=0`, result is **byte-identical** to `score_candidates_core(..None)` | — | — |
| B2 | `root_candidate_set_stable` | wrapper, K=3 | returned action set == the single-call action set (determinization adds/removes no AI action) | — | — |
| B3 | `ensemble_is_mean` | wrapper, K=2 (two fixed seeds) | ensemble score for an action == mean of the two per-sample scores | — | — |
| B4 | `near_zero_deadline_returns_floor` | wrapper, tiny shared deadline | returns the no-regression tactical floor, never empty/panic | — | K=3 all time out → still K-safe |
| **D** | `determinized_search_ignores_real_opponent_hand` (**the crux**) | `score_candidates_with_session` (cast pipeline, `card-test` recipe) | Craft a 2-player state where the opponent's **real** hand holds a specific reactive card. Baseline (K=0, sees real hand) action scores **differ** from K≥1 determinized scores → proves the path no longer keys off the real hand identity. | assert the opponent's real hand card is **absent by identity** from the determinized opp hand in ≥1 sample (the score delta is attributable to the swap, not noise) | if determinization were a no-op, D's revert assertion fails (scores would match) |

**Coverage honesty:** no parser/Oracle text is touched, so coverage numbers are unaffected. No `Effect::Unimplemented` short-circuit is involved; the crux test D's negative (scores differ) is paired with a positive reach-guard (real card actually swapped out), so it cannot pass vacuously.

**Maintainer-simulation matrix (hostile fixture → first production branch reached):**

| Hostile scenario | First branch in production code | Behavior |
|---|---|---|
| Opponent has no `deck_pools` | `unknown_hidden_pool` → `deck_pools.iter().find(..)` is `None` → empty pool → `continue` in loop | real state kept for that opponent (documented residual) |
| Empty unknown-slot set (all hidden cards revealed) | `slots.is_empty()` guard | `continue`; no resample |
| `pool.len() < slots.len()` | `zip` truncates | trailing slots keep real identity; debug log |
| Token in hidden zone | `unknown_slots` skips tokens (no DeckCardKey) | untouched |
| `private_look_player == opponent` (not AI) | `pinned_known_ids` excludes it | that peeked card **is** resampled (AI doesn't know it) |
| Two opponents (multiplayer) | `opponents_of` ordered walk | both determinized independently, one shared pool each |
| K=0 (Medium/WASM-capped) | wrapper early return | identical to today (B1) |

---

## 11. Rollout / Verification Protocol

1. `cargo fmt --all` (always direct).
2. **Tilt-first** compile/lint/test: `./scripts/tilt-wait.sh clippy test-ai` (fall back to direct phase-ai clippy/test only if Tilt is down). TypeScript unaffected (no FE change) but run `check-frontend` if the `.d.ts` is regenerated (it is **not** — no new WASM export; `get_ai_scored_candidates` signature is unchanged).
3. **Behavior-changing gate (mandatory, requirement #3):**
   - **Pre-commit quick paired-seed:** build an isolated `origin/main` baseline worktree, run `cargo ai-gate --games 10 --seed <S>` on both baseline and branch (same seed), attach the paired-seed markdown report. Because Medium is K=0, the **quick gate's default matchups run at Medium and should show zero delta**; add `--suite-filter` for a Hard/VeryHard matchup (or a temporary Hard preset) to exercise the K≥2 path and capture its report.
   - **Full tier → CI/nightly:** `cargo ai-gate --full-suite` (30×3). 
   - **Acceptance criterion:** **zero regressions** (no win-rate flips below baseline on any matchup); win-rate **may improve**. If any matchup regresses, do not refresh the baseline — investigate (likely the shallower per-sample depth; consider lowering K or raising the shared deadline).
   - Refresh baselines **only** with the paired-seed report attached (per CLAUDE.md AI policy).

---

## 12. Deferred follow-ups (named, so the slice is honest)

- **P6-followup-A — projection/combat determinization.** `combat_ai.rs:354-387` crackback and `threat_profile.rs:287` consume `project_to` (`projection.rs:113`), which simulates the opponent's turn from the **real** hand. These are *not* reached by `score_candidates`. Follow-up: feed `project_to` a determinized base (reuse `determinize_opponents`) and/or ensemble the projection. Separate machinery, separate diff.
- **P6-followup-B — DFC back-face hydration for sampled cards.** `apply_card_face_to_object` doesn't set `back_face` (needs `CardDatabase` rehydrate, not threaded into the scoring path). Follow-up: thread the DB or precompute `back_face` into pool `CardFace`s so sampled Adventure/MDFC opponent cards can be simulated as castable via their alternate face.

---

## 13. Risks

| Risk | Mitigation |
|---|---|
| **Strength regression** from shallower per-sample search (budget shared across K). | Node cap (not time) is the primary bound and usually leaves slack; shared deadline only truncates pathological positions. ai-gate acceptance = zero regressions; if it regresses, lower K or widen the shared deadline. Medium stays K=0. |
| **Latency** K× on complex positions. | Shared wall-clock `Deadline` hard-caps aggregate latency ~1500ms; iterative deepening degrades gracefully. Pipeline 4's latency gate (`choose_action` baseline) will catch aggregate regressions. |
| **Cross-process nondeterminism** (#4878). | Out of scope; ai-gate runs same-process. Determinizer itself is within-process deterministic (§8). |
| **`apply_card_face_to_object` side effects** (base_* mirrors, `base_characteristics_initialized`). | Determinized objects live only in hidden zones (no layer recompute); the primitive is the same one deck-load uses, so fields are populated consistently. |
| **Aggregation key instability** if `GameAction` equality is fragile. | Root candidate set is identical across samples; mirror the frontend `mergeScores` canonical-key approach; test B2 guards set stability. |
| **Multiplayer pool cost** (3+ opponents × K). | Per-preset K capped (multiplayer → 1 beyond a node-cost threshold); pool build is O(decklist) per opponent. |
| **Concurrent-agent collision** on `search.rs`/`config.rs`/`deck_knowledge.rs` (shared hot files). | Surgical edits only (split-not-rewrite of `score_candidates_with_session`; additive config field; additive `deck_knowledge` fn). Re-read before edit. |

---

## 14. Files touched (summary)

| File | Change |
|---|---|
| `crates/phase-ai/src/determinize.rs` | **NEW** — `determinize_opponents`, `pinned_known_ids`, `unknown_slots`, `opponents_of` + tests A1–A6, D. |
| `crates/phase-ai/src/lib.rs` | `pub mod determinize;`. |
| `crates/phase-ai/src/deck_knowledge.rs` | Split `accounted_object_ids` → `public_account_object_ids` (+ preserve existing fn); add `unknown_hidden_pool`. |
| `crates/phase-ai/src/search.rs` | Split `score_candidates_with_session` → `score_candidates_core(.., deadline_override)` + ensemble wrapper; seed/aggregate helpers + tests B1–B4. |
| `crates/phase-ai/src/planner/mod.rs` | `PlannerServices::with_deadline` (or `deadline: Option<Deadline>` param) so the wrapper can share one wall-clock ceiling; `None` = today's derivation. |
| `crates/phase-ai/src/config.rs` | Add `determinization_samples: usize` to the search-config struct; set per preset (Medium 0, Hard 2, VeryHard/CEDH 3, WASM `min(2,tier)`, multiplayer capped). |

**No engine-crate edits** beyond calling the existing `apply_card_face_to_object` (public). **No WASM export change. No frontend change. No engine visibility-rule change.**

---

## 15. CR annotations (grep-verified against `docs/MagicCompRules.txt`)

| CR | Verified text (grep) | Where annotated |
|---|---|---|
| **400.2** | "Library and hand are hidden zones, even if all the cards in one such zone happen to be revealed." | `determinize.rs` module doc + `unknown_hidden_pool`. |
| **401.2** | "Each library must be kept in a single face-down pile. Players can't look at or change the order of cards in a library." | shuffle/deal step (order randomization). |
| **401.3** | "Any player may count the number of cards remaining in any player's library at any time." | size-preservation assertions (hand + library len unchanged). |
| **402.1** | "The hand is where a player holds cards that have been drawn." (hand size countable ⇒ preserved) | `unknown_slots` (hand slot count preserved). |
| **701.20a** | "To reveal a card, show that card to all players for a brief time." | `pinned_known_ids` (revealed cards pinned). |

All five verified present in `docs/MagicCompRules.txt` (lines 1935, 1996, 1998, 2010, 3438 respectively) during planning.
