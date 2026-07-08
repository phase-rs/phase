# Pipeline 6 — Determinized Opponent Sampling on `deck_knowledge` (ROUND 2)

**Task:** Replace the AI planning path's perfect-information cheat (it sees the opponent's real hand + real library order during simulation) with **determinized sampling**: construct K plausible opponent hidden states consistent with public information + the sanctioned `deck_knowledge` model + engine reveal sets, run the existing search against each, and aggregate.

**Round-2 status:** Round-1 architecture was independently reviewed and judged fundamentally sound and correctly scoped. This document is a full standalone revision that carries forward every reviewer-confirmed element (RNG purity via clone, `#4878` discipline, budget story, WASM coherence, CR numbers, cheat-site inventory, K schedule shape, TT/ID analysis, deferred follow-ups A/B) and resolves the review's 1 blocker + 4 material gaps + 3 nits. Each resolution is tagged **[F1]**–**[F8]** inline where it lands.

**Scope decision (stated up front):** v1 determinizes **all opponents' hidden zones (hand + library together)** at the *root* of the `score_candidates` spell/priority planning path, and runs the existing (untouched) beam/rollout/quiescence search on each sample. This is a **complete, gate-passing, reviewable unit**. Two paths are explicitly **deferred to named follow-ups** (see §12) because they use *different machinery* (`project_to`, not `score_candidates`) and would bloat this diff:
- **P6-followup-A:** determinize the opponent-turn **projection** input consumed by `combat_ai.rs` crackback (`combat_ai.rs:354-387` → `project_to`) and by `threat_profile.rs:287`.
- **P6-followup-B:** honor engine **DFC back-face** hydration for sampled dual-faced cards (requires threading the `CardDatabase` into the scoring path; front-face cost/abilities are already correct in v1).

> **Why not a smaller "hand-only" slice?** From the AI's perspective the opponent's hand *and* library draw from the **same** unknown pool (`decklist − public zones − revealed`). Sampling "hand only" from `decklist − public − library` reproduces the *real* hand exactly (since real hand = decklist − public − library), hiding nothing. Hand and library must be redistributed **together** from the shared pool. So the coherent minimal slice is the full hidden-zone determinization; the genuine reductions are the two deferred *machinery* paths above.

---

## 0. Applicable skills (engine-planner Step 1)

| Skill | Applies? | Note |
|---|---|---|
| `add-ai-feature-policy` | **Partially — conventions only.** | This is **not** a `DeckFeatures` axis or a `TacticalPolicy`; the feature/policy scaffold does NOT apply. We adopt its non-negotiable phase-ai conventions: grep-verified CR annotations, band helpers where scores are involved (n/a here), the `no_name_matching` lint (n/a — no card-name classification), and the **`cargo ai-gate` paired-seed protocol** (§11). |
| `add-engine-variant` | **Yes — non-applicability confirmed.** | We add **no** engine enum variant. We add one plain `u32` config field (`determinization_samples`) to the phase-ai search-config struct — not an engine enum. `cargo engine-inventory` consulted: determinization is search infra, no `Effect`/`QuantityRef`/`TargetFilter`/etc. touched. Gate satisfied by non-applicability. |
| `card-test` | **Yes.** | The discriminating "cheat removed" runtime test (§10, test D) casts through the AI scoring path; follows the GameScenario/GameRunner conventions where a cast is exercised (`add_real_card` + `GameRunner::cast(..).resolve()` + `CastOutcome` deltas, verbatim Oracle text). |

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
- **Estimated impact:** every AI priority/spell-cast decision in every game (the `score_candidates` path), at difficulty tiers where `K > 0` (§7c). The cheat currently benefits the AI in *every* game with hidden cards; determinization removes it for the gated tiers.

---

## 3. Building Blocks (compose, don't reinvent)

| Building block | File:line | Use |
|---|---|---|
| `deck_knowledge::accounted_object_ids` | `deck_knowledge.rs:85-125` | **Refactor** into public-vs-hidden split (§6). Currently subtracts hand (a hidden zone) — wrong for opponent-pool sampling. |
| `deck_knowledge::DeckCardKey`, `deck_entry_key`, `object_key` | `deck_knowledge.rs:10-17, 127-141` | Reused verbatim for pool-conservation keying. |
| Decklist source | `deck_knowledge.rs:29` (`state.deck_pools.iter().find(...)`) — `PlayerDeckPool.current_main: Arc<Vec<DeckEntry>>` (ordered) | Ground-truth pool source. `DeckEntry.card` is a full `CardFace` (abilities/cost/types) — no DB needed. |
| `apply_card_face_to_object` | `printed_cards.rs:92-237` | In-place identity overwrite of an existing `GameObject` from a `CardFace`. See the **exact-scope note below** — round-1's "sets all castability fields" claim is corrected. |
| `printed_ref_from_face` | `printed_cards.rs:64-72` | Not needed directly (`apply_card_face_to_object` sets `printed_ref`), but confirms identity handle derivation. |
| `engine::game::derived` continuous-reveal sync | `derived.rs:271` `sync_continuous_library_top_reveals`, `derived.rs:316` `sync_continuous_hand_reveals` | **[F4]** Extract these two private fns into one public `sync_continuous_reveals(&mut GameState)`; the determinizer calls it on its clone so `revealed_cards` reflects continuous reveal statics regardless of caller derive state (§7a). |
| Engine RNG `ChaCha20Rng` | `game_state.rs:5864-5867`, `default_rng` `:42-44` | Seed source; cross-platform deterministic (native+WASM). |
| Shuffle primitive | `rand::seq::SliceRandom::shuffle` on a `Vec` (as used engine-side, e.g. `ability_utils.rs:1259`) | Seeded shuffle of the ordered pool. |
| `planner::quick_state_hash` | `planner/mod.rs:96-216` | Per-decision seed mixing (varies samples by position). Public within-crate. Hashes `hand` by ObjectId, `library` by **length only** (`mod.rs:140-144`) — relevant to §7b seed design and to F3 below. |
| `Deadline` | `engine/src/util/deadline.rs` (`web_time::Instant`, `Copy`) | Shared wall-clock ceiling across K samples. |
| `score_candidates_with_session` | `search.rs:1484` | Split into ensemble wrapper + core (§7). |

**[F2] Exact scope of `apply_card_face_to_object` — round-1 correction.** Verified by reading `printed_cards.rs:92-237`: the primitive overwrites `name`, `power`/`toughness`/`loyalty`/`defense` (+ `base_*` mirrors), `card_types`, `mana_cost`, `keywords`, `abilities`/`triggers`/`replacements`/`static_definitions` (+ `base_*`), `color`, `cleave_variant`, `printed_ref`/`base_printed_ref`, `source_related_token_ids`, `spellbook`, `modal`, `additional_cost`, `strive_cost`, `casting_restrictions`, `casting_options`, and (conditionally) `class_level`/`intensity`/`case_state`/`room_unlocks`/`attraction_lights`. It is **not** the total identity-swap the round-1 §3 claimed. It does **NOT** rewrite the following residual fields, each justified inert in the v1 score path with cited evidence:

| Residual field (not overwritten) | Why it stays stale | Why inert in v1 score path (evidence) | Follow-up trip-wire |
|---|---|---|---|
| `card_id` | Assigned at deck load (`printed_cards.rs`/`deck_loading.rs`), never touched here. | The v1 scoring path performs **zero `CardDatabase` lookups keyed by `card_id`**; cast actions reference the object and read its (overwritten) characteristics self-consistently (`casting.rs:1037`, `casting.rs:7681`). A stale `card_id` therefore never resolves the object back to its real identity within scoring. | If any future score-path consumer resolves an object via `card_id → CardDatabase`, this becomes a cheat leak — guarded by a `debug_assert!`-backed invariant comment on the determinizer and the D crux test (§10). |
| `back_face` | Needs a `CardDatabase` rehydrate not threaded into scoring. | Front-face cost/abilities are correct; alternate-face casting stays inert. | **P6-followup-B** (explicit). |
| `perpetual_mods` | Persist across hidden zones **by design** (`game_object.rs:393-404`). | **Deliberately preserved, not reset.** Blanking a perpetual edit would (a) fight the documented engine invariant that these follow the card through hidden zones, and (b) force the determinizer to make a visibility judgment the engine does not currently make. Rare (digital-only Alchemy); not read by candidate-gen or eval for hidden-zone objects in v1. | If Alchemy perpetual state ever influences hidden-zone candidate gen, revisit. |
| `intensity` | Same persistence contract as `perpetual_mods` (`game_object.rs:393-397`); `apply_card_face_to_object` only *seeds* it when `== 0`. | Same as `perpetual_mods` — deliberately preserved. | Same as above. |
| `counters` | Not reset by the primitive. | Hidden-zone objects (hand/library) do not carry battlefield counters in normal play; candidate gen/eval read counters only for battlefield permanents (`quick_state_hash` counter-fold is battlefield-scoped, `planner/mod.rs:189-213`). | Hostile fixture (§10 A7) asserts a residual counter on a resampled hidden card does not perturb the returned score set. |
| `stickers` | Not reset by the primitive. | Not read by the v1 score-path candidate gen/eval for hidden-zone objects. | Revisit if sticker state feeds hidden-zone scoring. |
| `casting_permissions` | Not reset by the primitive. | Governs *whether* a specific object may be cast; for an unknown hidden card being resampled, no AI candidate references it by identity (see the F5 pin-invariant, §7b), so a stale permission cannot enable an illegal cheat candidate. | Revisit if permissions gate hidden-zone candidate enumeration. |

**Decision (F2): option (b) — enumerate + justify, do not defensively reset.** A blanket reset is *less* maintainable and *less* correct here: `card_id` cannot be reset to a correct value (we do not carry the sampled `CardFace`'s deck-load id), and `perpetual_mods`/`intensity` persist across hidden zones by explicit design — zeroing them fights engine semantics and makes a visibility call the engine itself declines to make. The table above is the maintainable, self-documenting contract; the `debug_assert!` invariant + crux test D + hostile fixture A7 keep it honest.

**One new module** is justified: `crates/phase-ai/src/determinize.rs` — the `Determinizer` is a composable primitive (a `&GameState → GameState` sampler), reused by v1's search path and by both follow-ups. It is not a one-off.

---

## 4. Logic Placement

| Piece | Layer | Justification |
|---|---|---|
| Sampling pool computation (`unknown_hidden_pool`) | `phase-ai/deck_knowledge.rs` | Extends the existing sanctioned knowledge model; keeps the "what the AI may know about a deck" logic in one module. |
| Determinizer (state resample) | **new** `phase-ai/determinize.rs` | AI-only simulation concern. **Not** an engine visibility rule — the engine's authoritative `GameState`/`filter_state_for_viewer`/multiplayer `SimulationFilter` are **untouched** (requirement #5). The AI builds its *own* sampled state. |
| Continuous-reveal sync (`sync_continuous_reveals`) | `engine/game/derived.rs` (make public) | **[F4]** Reveal-set derivation is an **engine** visibility concern (CR 400.2/701.20a). The determinizer must not recompute visibility itself — it calls the engine's authoritative sync. Extraction is a minimal, additive refactor of two existing private fns; the engine keeps ownership of the rule. |
| Ensemble loop + aggregation + budget split | `phase-ai/search.rs` (`score_candidates_with_session` wrapper) | Sits at the existing scoring seam; all callers (native + WASM) inherit it. |
| `determinization_samples` config field + per-preset values | `phase-ai/config.rs` | Difficulty/platform gating lives with the other search knobs (`max_depth`/`max_nodes`/`time_budget_ms`). |
| `--difficulty` gate override | `phase-ai/bin/ai_gate.rs` (+ it already flows through `SuiteOptions::new`) | **[F1]** The gate must be able to exercise a `K > 0` tier; difficulty is a suite-level knob, so it belongs in the gate binary's arg parsing. |
| Object identity overwrite | reuse `engine::game::printed_cards::apply_card_face_to_object` | Engine owns object construction; we call its primitive, we do not duplicate it. |

No frontend change required — the WASM worker path already calls `score_candidates_with_session`; the ensemble becomes transparent (the frontend's own `mergeScores` still averages across workers on top, which is additive and fine).

---

## 5. Rust Idioms

- **No bool flags.** `determinization_samples: u32` **[F6]** where `0` is the disabled sentinel — matching the existing `max_nodes`/`rollout_samples`/`time_budget_ms` numeric-knob convention in `config.rs` (all `u32`), *not* `usize`. An integer count, not a bool.
- **Ordered iteration only** for determinism (see §8): collect slots from `player.hand`/`player.library` (`Vec`s) and the pool from `deck_pools.current_main` (ordered `Arc<Vec<DeckEntry>>`); **never** iterate `state.objects` (`HashMap`) where order affects output (guards against the #4878 class of cross-process nondeterminism).
- **Typed identity handles:** reuse `DeckCardKey` (already `Eq + Hash`) for pool conservation; reuse `HashSet<ObjectId>` for the pinned-known set.
- **Exhaustive handling** of the pinned-vs-unknown partition (a card is pinned iff its id ∈ known set), no wildcard fallthrough.
- Determinizer returns an owned `GameState` (search already clones per call), matching `apply_candidate`'s clone-and-mutate idiom.
- Aggregation keyed by a typed `GameActionKey` (canonical serialization mirroring the frontend `mergeScores` `JSON.stringify`), not by float score.

---

## 6. `deck_knowledge` refactor — the sampling pool

**Problem:** `accounted_object_ids` (`deck_knowledge.rs:85-125`) subtracts the player's **hand** (line 89) — correct for "cards remaining in *my* library" but wrong for sampling an *opponent's* combined hidden pool, because it removes the very cards we need to redistribute.

**[F8] Caller reality (round-1 §6 correction).** Verified by grep: `known_remaining_deck_counts` (`deck_knowledge.rs:25`) has **no external callers** — it is invoked only inside `deck_knowledge.rs` itself (by `remaining_deck_view` at `:61`, plus unit tests at `:189/:236/:276`). The real external consumers are `policies/tutor.rs:36` and `threat_profile.rs:287`, both of which go through **`remaining_deck_view`** (`deck_knowledge.rs:60`), not `known_remaining_deck_counts` directly. The refactor therefore preserves `known_remaining_deck_counts`' behavior purely to keep `remaining_deck_view` (and thus tutor/threat_profile) byte-identical; there is no direct external caller to worry about.

**Refactor (surgical):**
1. Split `accounted_object_ids` into `public_account_object_ids(state, player)` — graveyard + owned-battlefield + owned-exile + stack-spells (lines 90-122, **excluding** the hand line 89 and never touching library) — and keep `accounted_object_ids` as `public_account_object_ids(...) ++ player.hand` so `known_remaining_deck_counts` (and through it `remaining_deck_view` → tutor/threat_profile) is behavior-preserved.
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
///
/// IDENTITY-SWAP CAVEAT (see §3/F2): `apply_card_face_to_object` does not rewrite
/// `card_id`/`back_face`/`perpetual_mods`/`intensity`/`counters`/`stickers`/
/// `casting_permissions`. The v1 score path is verified identity-blind to all of
/// these (zero card_id-keyed CardDatabase lookups; casting reads overwritten
/// object characteristics). The debug_assert below documents the card_id invariant.
pub fn determinize_opponents(
    state: &GameState,
    ai_player: PlayerId,
    rng: &mut ChaCha20Rng,
) -> GameState {
    let mut sim = state.clone();
    // [F4] Ensure `revealed_cards` reflects continuous reveal statics (Future
    // Sight top-of-library, "play with hands revealed") regardless of whether the
    // caller ran a derive pass. Engine owns the visibility rule; we invoke it.
    engine::game::derived::sync_continuous_reveals(&mut sim);   // CR 400.2 / 701.20a
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
                debug_assert!(obj.zone == Zone::Hand || obj.zone == Zone::Library,
                    "determinizer only overwrites hidden-zone objects");
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
- **[F4] Reveal fidelity via the engine.** `revealed_cards` is a **derived** field that `apply_action` clears at each action boundary and only `derive_display_state` re-populates (via `sync_continuous_*`). Verified: `derive_display_state` is an **export-path** function (`engine-wasm/src/lib.rs:1757`, tests) — it is **not** guaranteed to have run on the state that `get_ai_scored_candidates` scores. Reading `revealed_cards` blindly would therefore let the AI resample a card the opponent legitimately shows (Future Sight top card; a hand revealed by a continuous static), violating CR 400.2/701.20a. The determinizer closes this by calling the engine-owned `sync_continuous_reveals` on its clone **before** `pinned_known_ids`. We deliberately do **not** call the full `derive_display_state` (it runs an expensive board-global mana-availability sweep, `derived.rs:64+`, wrong layer + K× cost); the dedicated sync is cheap and sufficient. `public_revealed_cards` (one-shot reveals, never cleared — `game_state.rs:6748-6750`) needs no re-sync and is read directly.
- **ObjectIds and zone Vecs are never mutated** — only object *identity* fields are overwritten in place. Library positions are preserved as slots; the *identities* assigned to library slots are shuffled, which is exactly the CR 401.2 "order unknown" randomization at the identity level. Determinized objects only ever live in hand/library (hidden zones) → no layer recompute needed.
- **DFC caveat (v1):** `apply_card_face_to_object` does not set `back_face` (needs DB rehydrate). Sampled dual-faced opponent cards have correct front-face cost/abilities; alternate-face casting stays inert → **P6-followup-B**. Documented in the module doc comment (and §3/F2).

`pinned_known_ids` (in `determinize.rs`):

```rust
/// CR 400.2 / 701.20a: the set of `opponent` object ids whose identity `ai_player`
/// legitimately knows. Read from engine-derived reveal state (post `sync_continuous_reveals`).
fn pinned_known_ids(state: &GameState, ai_player: PlayerId, opponent: PlayerId) -> HashSet<ObjectId> {
    let mut ids: HashSet<ObjectId> = HashSet::new();
    ids.extend(state.revealed_cards.iter().copied());          // continuous + momentary (post-sync)
    ids.extend(state.public_revealed_cards.iter().copied());   // one-shot, never cleared
    if state.private_look_player == Some(ai_player) {          // CR 701.20e: only the looker
        ids.extend(state.private_look_ids.iter().copied());
    }
    ids  // membership-only; never iterated for output order (§8)
}
```

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
    // Seed: fixed across K for a given (position, game/worker); per-sample split by i.
    let base_seed = quick_state_hash(state)              // varies by position (per-decision)
        .wrapping_add(state.rng_seed)                    // varies by game (native distinct rng_seed)
        .wrapping_add(state.rng.clone().next_u64());     // varies by worker (WASM re-seeds state.rng)
    let mut acc: Vec<(GameAction, f64)> = Vec::new();    // first-seen order preserved
    let mut counts: HashMap<GameActionKey, usize> = HashMap::new();
    for i in 0..k {
        let mut rng = ChaCha20Rng::seed_from_u64(base_seed.wrapping_add(splitmix64(i as u64)));
        let sampled = determinize_opponents(state, ai_player, &mut rng);
        let scored = score_candidates_core(&sampled, ai_player, config, session, Some(deadline));
        merge_into(&mut acc, &mut counts, scored);       // sum per action
    }
    finalize_mean(acc, counts, k as usize)               // divide sums by observed count; assert count==K
}
```

- **Aggregation = mean** per action (matches the frontend `mergeScores` contract). Root candidate set is **identical across samples** (determinization changes only opponent hidden zones, never the AI's own legal actions), so every action is observed in all K samples; mean is well-defined. Key actions by their existing equality / canonical serialization, mirroring `mergeScores`' `JSON.stringify`.
- **[F5] Pin-invariant (stated explicitly, then enforced):**
  1. **Invariant:** any opponent card the AI can enumerate as a candidate reference (a target, a discard/sacrifice victim it names, a card it may choose) is *necessarily* public (on the battlefield, on the stack, in a public zone) or pinned-revealed. Determinization swaps **only** unknown hidden cards, and "unknown" ≡ "not public **and** not in `pinned_known_ids`" — which is exactly the complement of the AI-enumerable set. Therefore no swapped identity can ever appear in a candidate the AI generates, so the root candidate set is identical across all K samples by construction. (The AI cannot target an opponent's hidden hand card by identity; a "target player discards" candidate names the *player*, not a hidden card, and the victim is chosen at resolution — not part of the AI's enumerated candidate.)
  2. **Enforcement:** `finalize_mean` takes `k` and `debug_assert!`s that every accumulated action's observed count `== k`. If a future change ever lets the candidate set drift across samples (strategy fusion averaging over a non-constant support), the assert fires loudly in debug/test instead of silently averaging heterogeneous supports. In release it degrades to per-action-observed-count mean (never a divide-by-zero; `counts` is always ≥ 1 for any accumulated action).
  3. **Test:** B2 (§10) drives a decision whose candidate set **references an opponent permanent** (public) and asserts the returned action set is identical to the single-call set across K=3 — a discriminating fixture, not generic set-equality.
- **Seed source (reviewer-confirmed RNG purity via clone):** `quick_state_hash(state) ⊕ state.rng_seed ⊕ state.rng.clone().next_u64() ⊕ splitmix64(i)`. The `state.rng.clone()` keeps `&GameState` immutable (the wrapper takes `&state`). Native runs get distinct ensembles via distinct `rng_seed` per game; WASM workers diverge via the per-worker `state.rng` re-seed (`lib.rs:1303`). Within-process reproducible given identical state+seed (requirement #2). Cross-process byte-identity remains gated by #4878 (acknowledged, out of scope).

### 7c. `crates/phase-ai/src/config.rs` — gating

Add `pub determinization_samples: u32` **[F6]** to the per-difficulty search-config struct (the one holding `max_depth`/`max_nodes`/`max_branching`/`time_budget_ms`). Per-preset values:

| Preset | K | Rationale |
|---|---|---|
| Medium (`config.rs:780`) | **0** | Default tier: keep perfect-information search (see sign-off below). |
| Hard (`:803`) | **2** | 2 samples ≈ halves single-sample variance at 2× base cost; node cap 48 keeps each search short. **This is the tier the quick ai-gate exercises (§11).** |
| VeryHard (`:826`) | **3** | 3 samples materially de-bias without runaway cost; node cap 64. |
| CEDH (`:849`) | **3** | Same as VeryHard; multiplayer + node cap 96 already dominates cost. |
| WASM override (`:885-889`) | **`min(2, tier)`** | The frontend worker pool **already** provides cross-sample root parallelism (`ai-worker-pool.ts` merges N workers). Cap per-worker K at 2 so effective samples = N_workers × K without per-worker latency blow-up. |
| Multiplayer (`:910-945`) | inherit tier, but **1** if `players > 2` beyond a node-cost threshold | Determinizing 3+ opponents per sample multiplies pool work; keep K modest. |

**[F1] Design sign-off — determinization is a higher-tiers-only feature (Medium stays K=0).** This is a deliberate, defended choice, not an oversight:
1. **Play-strength floor.** Medium is the **default** difficulty for casual/new players. Determinization reduces the *effective* per-sample search depth under a shared budget (K searches share one wall-clock ceiling), which can *lower* raw play strength on the tier most players face. Keeping Medium at perfect-information search preserves the established default-tier strength floor.
2. **Realism is a "harder AI" feature.** Removing the hidden-info cheat makes the AI play *more like a human who cannot see your hand* — a realism/fairness improvement most valuable at the tiers marketed as tougher/fairer opponents. Gating it to Hard+ matches player expectation.
3. **Compute budget.** Medium runs on the widest hardware range (including mobile/WASM defaults). A K× cost multiplier on the cheapest tier is the wrong place to spend it; Hard+ players opt into more compute.
4. **Gate implication (see §11):** because Medium is K=0, the quick ai-gate's *default* Medium matchups would show **zero delta** and ship the feature untested — so the gate protocol **must** run at `--difficulty hard`, and the paired origin/main baseline must run the **same** Hard preset for a fair pair.

**K justification (requirement #4):** K∈{2,3} is the empirically standard low end for determinized search where per-sample cost is high; it captures most of the variance-reduction benefit (variance ∝ 1/K) before diminishing returns, and keeps the multiplier bounded. **Multiplication happens at the root (ensemble of K full searches), not per-node** — per-node determinization would re-randomize mid-search and destroy the TT/eval-cache consistency that keys on fixed identities within a search.

**Perf budget argument (requirement #4):** the **node cap (24–96) is the primary bound** and is deterministic regardless of time; a single search typically completes well under 1500ms. K sequential searches therefore usually finish in K×(sub-cap time) ≪ K×1500ms. The **shared wall-clock `Deadline`** (created once in the wrapper, threaded into all K via `deadline_override`) caps *aggregate* latency at ~1500ms: early fast samples leave budget for later ones; if a sample is pathologically slow, later samples' iterative deepening returns their deepest-completed rung (or the no-regression tactical floor at `search.rs:1694-1697`) — **never empty, never a crash**. Iterative deepening thus provides graceful degradation exactly as it does today, now shared across the ensemble.

**TT/eval-cache interaction (requirement re: commit 96e50bcf5) + [F3] session-cache contamination:**
- Each sample runs in its **own** `PlannerServices` (fresh per `score_candidates_core` call) → its own fresh 4096-entry TT and eval_cache. `search_position_hash` folds full per-player **library ObjectId order** (`planner/mod.rs:261-266`); since determinization keeps object *IDs* in place and only swaps identities, the per-sample hash is internally consistent, and the separate per-sample TTs mean **zero cross-sample contamination** (the eval_cache `quick_state_hash` aliasing risk cannot occur across samples because caches are not shared). Correct-by-construction; no TT change needed.
- **[F3] `AiSession`-carried cache is NOT populated from sampled state in the score path.** The same `session: &Arc<AiSession>` is passed to all K `score_candidates_core` calls. `AiSession` carries a turn-scoped `projection_cache: Arc<RwLock<HashMap<ProjectionKey, ..>>>` (`session.rs:43-46`) whose `ProjectionKey.state_hash` (`projection.rs:93-100`) would **collide across determinized worlds** (library is hashed by length only — `planner/mod.rs:140-144` — so two worlds differing only in library *identities* map to the same key). This is safe in v1 **by scope, and now stated explicitly**: the score path **never calls `project_to`** (verified: zero `project_to`/`projection_cache` references in `search.rs` and `planner/mod.rs`), so nothing in the ensemble ever reads *or writes* `projection_cache` from sampled state. The only writer is `combat_ai` on the projection path, which v1 does not touch. **Follow-up A MUST use a per-sample or cleared `projection_cache`** (or fold the sampled-world identity into `ProjectionKey`) — recorded as a hard prerequisite in §12 so the latent collision is closed the moment the projection path is determinized. The other `AiSession` maps (`features`/`plan`/`strategy`/`synergy`/`deck_profile`) are keyed by `PlayerId` from the *decklist* (`session.rs:65-77`), which determinization never changes, so they remain valid across samples.

---

## 8. Determinism (requirement #2) — the #4878 discipline

- All ordering-sensitive collection uses **ordered `Vec`s** (`player.hand`, `player.library`, `deck_pools.current_main`), never `state.objects`/`HashMap` iteration. The seeded `ChaCha20Rng` shuffle is applied to a Vec whose pre-shuffle order is decklist order → deterministic within-process.
- The pinned-known set is a `HashSet<ObjectId>` used only for **membership tests** (order-irrelevant), never iterated to produce output order.
- Seeds derive from `quick_state_hash ⊕ rng_seed ⊕ rng.clone() draw ⊕ splitmix64(i)` — reproducible given identical state+seed in-process.
- **Cross-process** byte-identity is *not* guaranteed (GH #4878: `HashMap`/`HashSet` `RandomState` leaks into AI tie-breaking upstream of us). Acknowledged and out of scope; the ai-gate paired-seed protocol runs same-process so it is unaffected.

---

## 9. Extension vs Creation

- **Extends** the existing "prepare state → score" seam (WASM already re-seeds `state.rng` before scoring), the `deck_knowledge` module (public/hidden split + pool), the engine `derived` module (one new public sync wrapper over two existing private fns), and reuses `apply_card_face_to_object`.
- **Creates** exactly one new module (`determinize.rs`) — justified as a reusable primitive consumed by v1 and both follow-ups.
- **No engine variant, no new engine type, no engine visibility-rule change** (requirement #5) — the `sync_continuous_reveals` extraction is a visibility fn made callable, not a new rule.

---

## 10. Verification Matrix (discriminating runtime tests)

All tests in `determinize.rs` `#[cfg(test)]` unless noted. Runtime tests drive the production `determinize_opponents` / `score_candidates_with_session` entry points (not hand-built shapes).

| # | Test | Seam / production entry | Revert-failing assertion | Positive reach-guard | Hostile / negative sibling |
|---|---|---|---|---|---|
| A1 | `preserves_public_and_own_zones` | `determinize_opponents` | opponent battlefield/graveyard/exile **and** AI's own hand/library are identity-identical after determinize | assert ≥1 opponent hidden card **did** change (else vacuous) | — |
| A2 | `preserves_hidden_zone_sizes` (CR 401.3) | `determinize_opponents` | `opp.hand.len()` and `opp.library.len()` unchanged | as A1 | opponent with 0-card hand / empty library → no panic |
| A3 | `pins_revealed_cards` (CR 400.2/701.20a) | `determinize_opponents` | put an opp hand-card id in `public_revealed_cards`; its identity is **unchanged** after determinize | assert a *different* unknown opp card **did** change (proves resampling ran) | id in `revealed_cards`; id in `private_look_ids` w/ `private_look_player==ai` (pinned) vs `==opponent` (NOT pinned) |
| **A3b** | `pins_continuous_reveal_static` **[F4]** (CR 400.2/701.20a) | `determinize_opponents` on a **non-derived** input state | opponent controls/has a continuous reveal static (`RevealHand{who:Opponents}` cast at them, or `RevealTopOfLibrary`); build the input state **without** running `derive_display_state`; assert the statically-revealed opp card(s) are **unchanged** after determinize | assert a *different*, non-revealed opp hidden card **did** change | if `sync_continuous_reveals` is NOT called in the determinizer, the revealed card gets resampled → this assertion fails (proves the F4 fix is load-bearing) |
| A4 | `samples_only_from_decklist` (conservation) | `determinize_opponents` | every determinized hidden identity ∈ decklist multiset; (public ∪ hidden) multiset == decklist | — | token in opponent hand → left untouched (no DeckCardKey) |
| A5 | `seed_deterministic_and_position_varying` | wrapper seed path | same (state,seed) → identical sample; two different states → different sample | — | — |
| A6 | `empty_deck_pool_is_noop` | `determinize_opponents` | opponent without `deck_pools` entry → state unchanged | — | pool smaller than unknown slots → leftover slots keep real identity, no panic |
| **A7** | `residual_fields_do_not_leak` **[F2]** | `determinize_opponents` + `score_candidates_with_session` | stamp a stale `counters`/`perpetual_mods` entry on an opp hidden card, resample it; assert the field is left as-is (documents the deliberate non-reset) **and** the returned score set is unchanged vs a run without the stamp (proves inertness in the score path) | assert the card's *identity* fields (name/cost) did change (resample ran) | `card_id` stale after swap → assert no score-path `CardDatabase` lookup resolves it to the real card (score set identity-blind) |
| B1 | `k0_equals_baseline` | `score_candidates_with_session` | with `determinization_samples=0`, result is **byte-identical** to `score_candidates_core(..None)` | — | — |
| B2 | `root_candidate_set_stable_over_opponent_permanent` **[F5]** | wrapper, K=3 | craft a decision whose candidate set **references an opponent permanent** (e.g., a removal/targeting action against an opp battlefield creature); returned action set == the single-call action set across all 3 samples (determinization adds/removes no AI action) | — | opponent hidden hand fully resampled between samples, yet candidate set identical → proves the pin-invariant |
| B3 | `ensemble_is_mean` | wrapper, K=2 (two fixed seeds) | ensemble score for an action == mean of the two per-sample scores | — | `finalize_mean` `debug_assert` observed-count==K holds |
| B4 | `near_zero_deadline_returns_floor` | wrapper, tiny shared deadline | returns the no-regression tactical floor, never empty/panic | — | K=3 all time out → still K-safe |
| **D** | `determinized_search_ignores_real_opponent_hand` (**the crux**) | `score_candidates_with_session` (cast pipeline, `card-test` recipe) | **[F7]** Craft a 2-player state where the opponent's **real** hand holds **Negate** — verbatim Oracle text *"Counter target noncreature spell."* — with untapped mana to cast it, and the AI is deciding whether to cast a noncreature spell (e.g., a mana rock) into that open mana. Baseline (K=0, sees the real Negate) action scores **differ** from K≥1 determinized scores → proves the path no longer keys off the real hand identity. | assert **Negate is absent by identity** from the determinized opp hand in ≥1 sample (the score delta is attributable to the swap, not noise) | if determinization were a no-op, D's revert assertion fails (scores would match) |

> **Card-test note for D (F7):** the implementer must confirm **Negate** is present in the test card fixture (`add_real_card("Negate")`) or substitute the nearest engine-supported reactive noncreature counterspell and quote *its* verbatim Oracle text. The requirement is a **named, quoted** reactive card whose presence in the opponent's hidden hand changes the AI's own cast valuation — not a generic "reactive card."

**Coverage honesty:** no parser/Oracle text is touched, so coverage numbers are unaffected. No `Effect::Unimplemented` short-circuit is involved; the crux test D's negative (scores differ) is paired with a positive reach-guard (real card actually swapped out), and A3b/A7 each pair their negative with a positive resample-ran guard, so none can pass vacuously.

**Maintainer-simulation matrix (hostile fixture → first production branch reached):**

| Hostile scenario | First branch in production code | Behavior |
|---|---|---|
| Opponent has no `deck_pools` | `unknown_hidden_pool` → `deck_pools.iter().find(..)` is `None` → empty pool → `continue` in loop | real state kept for that opponent (documented residual) |
| Empty unknown-slot set (all hidden cards revealed) | `slots.is_empty()` guard | `continue`; no resample |
| `pool.len() < slots.len()` | `zip` truncates | trailing slots keep real identity; debug log |
| Token in hidden zone | `unknown_slots` skips tokens (no DeckCardKey) | untouched |
| Continuous reveal static, non-derived input | `sync_continuous_reveals` populates `revealed_cards`, then `pinned_known_ids` | statically-revealed card pinned (A3b) |
| `private_look_player == opponent` (not AI) | `pinned_known_ids` excludes it | that peeked card **is** resampled (AI doesn't know it) |
| Stale residual field (counter/perpetual/card_id) | `apply_card_face_to_object` leaves it; score path is identity-blind | no leak (A7) |
| Two opponents (multiplayer) | `opponents_of` ordered walk | both determinized independently, one shared pool each |
| K=0 (Medium/WASM-capped) | wrapper early return | identical to today (B1) |

---

## 11. Rollout / Verification Protocol

1. `cargo fmt --all` (always direct).
2. **Tilt-first** compile/lint/test: `./scripts/tilt-wait.sh clippy test-ai` (fall back to direct phase-ai clippy/test only if Tilt is down). Also `test-engine` because `derived.rs` gains a public fn (F4 extraction). TypeScript unaffected (no FE change); the `.d.ts` is **not** regenerated (no new WASM export; `get_ai_scored_candidates` signature unchanged).
3. **Behavior-changing gate (mandatory, requirement #3) — [F1] must exercise `K > 0`:**
   - **CLI change (permanent, on branch):** add a `--difficulty {medium|hard|veryhard|cedh}` flag to `ai_gate.rs` `parse_args` (`Args` gains a `difficulty: AiDifficulty` field, default `Medium`), threaded into `SuiteOptions::new(args.difficulty, args.games, args.seed)` at `ai_gate.rs:53`. `SuiteOptions.difficulty` and `run_suite` already apply one uniform difficulty (`run.rs:161`, `:191`) — no `run.rs` behavior change, just pass the parsed value.
   - **Quick paired-seed gate (the required run):** build an isolated `origin/main` baseline worktree. Run **both** baseline and branch with **`--difficulty hard`** and the same seed:
     `cargo ai-gate --difficulty hard --games 10 --seed <S>` on the branch, and the equivalent on the baseline. This exercises the Hard preset (K=2 on the branch, K=0/perfect-info on origin/main), so the pair isolates **exactly the determinization delta**. Attach the paired-seed markdown report.
   - **[F1] Baseline-binary caveat (must-do):** origin/main's `ai_gate.rs` predates the `--difficulty` flag, so its binary cannot accept it. In the **disposable baseline worktree**, apply the one-line override so the pair runs at the same preset — either cherry-pick just the `ai_gate.rs` arg-parsing hunk, or edit `ai_gate.rs:53` in that worktree to `SuiteOptions::new(AiDifficulty::Hard, ...)`. Because `determinization_samples` does not exist on origin/main, Hard there is K=0 with identical node caps/depth to the branch's Hard, giving a clean K=0-vs-K=2 pair. **Do not** compare branch-Hard against baseline-Medium — that would confound difficulty with determinization.
   - **Full tier → CI/nightly:** `cargo ai-gate --full-suite` (30×3), optionally `--difficulty veryhard` to stress the K=3 tier.
   - **Acceptance criterion:** **zero regressions** (no win-rate flips below baseline on any matchup); win-rate **may improve**. If any matchup regresses, do not refresh the baseline — investigate (likely the shallower per-sample depth; consider lowering K or raising the shared deadline).
   - Refresh baselines **only** with the paired-seed report attached (per CLAUDE.md AI policy).

---

## 12. Deferred follow-ups (named, so the slice is honest)

- **P6-followup-A — projection/combat determinization.** `combat_ai.rs:354-387` crackback and `threat_profile.rs:287` consume `project_to` (`projection.rs:113`), which simulates the opponent's turn from the **real** hand. These are *not* reached by `score_candidates`. Follow-up: feed `project_to` a determinized base (reuse `determinize_opponents`) and/or ensemble the projection. **[F3] Hard prerequisite:** because `project_to` writes `AiSession.projection_cache` keyed by a `state_hash` that folds library by length only (`planner/mod.rs:140-144`), determinized worlds collide in that cache. Follow-up A **must** use a per-sample or cleared `projection_cache`, or extend `ProjectionKey` with the sampled-world identity, before determinizing the projection input — otherwise sampled projections cross-contaminate. Separate machinery, separate diff.
- **P6-followup-B — DFC back-face hydration for sampled cards.** `apply_card_face_to_object` doesn't set `back_face` (needs `CardDatabase` rehydrate, not threaded into the scoring path). Follow-up: thread the DB or precompute `back_face` into pool `CardFace`s so sampled Adventure/MDFC opponent cards can be simulated as castable via their alternate face. (Also revisits the `card_id` staleness trip-wire from §3/F2 if DB-keyed lookups enter the score path.)

---

## 13. Risks

| Risk | Mitigation |
|---|---|
| **Strength regression** from shallower per-sample search (budget shared across K). | Node cap (not time) is the primary bound and usually leaves slack; shared deadline only truncates pathological positions. ai-gate acceptance = zero regressions; if it regresses, lower K or widen the shared deadline. Medium stays K=0 (F1 sign-off). |
| **Latency** K× on complex positions. | Shared wall-clock `Deadline` hard-caps aggregate latency ~1500ms; iterative deepening degrades gracefully. Pipeline 4's latency gate (`choose_action` baseline) will catch aggregate regressions. |
| **[F4] Missing continuous-reveal pin** if the score-path state is pre-derive. | Determinizer calls engine-owned `sync_continuous_reveals` on its clone before pinning; A3b drives a non-derived input to prove the fix is load-bearing. |
| **[F2] Residual identity fields leak** the real card (card_id/back_face/etc.). | Verified inert in v1 score path (zero card_id-keyed DB lookups; casting reads overwritten characteristics); `debug_assert` invariant + A7 hostile fixture + follow-up B trip-wire. |
| **[F3] `AiSession.projection_cache` cross-sample collision.** | Not reached in v1 (score path never calls `project_to`; verified); stated explicitly and made a hard prerequisite for follow-up A. |
| **[F5] Aggregation instability** if the candidate set drifts across samples. | Pin-invariant guarantees a constant support; `finalize_mean` `debug_assert`s observed-count==K; B2 proves stability over an opponent-permanent-referencing decision. |
| **Cross-process nondeterminism** (#4878). | Out of scope; ai-gate runs same-process. Determinizer itself is within-process deterministic (§8). |
| **`apply_card_face_to_object` side effects** (base_* mirrors, `base_characteristics_initialized`). | Determinized objects live only in hidden zones (no layer recompute); the primitive is the same one deck-load uses, so populated fields are consistent. |
| **[F1] Gate ships feature untested** (Medium K=0). | Gate protocol mandates `--difficulty hard` on both branch and baseline worktree; baseline-binary caveat spelled out. |
| **Multiplayer pool cost** (3+ opponents × K). | Per-preset K capped (multiplayer → 1 beyond a node-cost threshold); pool build is O(decklist) per opponent. |
| **Concurrent-agent collision** on `search.rs`/`config.rs`/`deck_knowledge.rs`/`derived.rs`/`ai_gate.rs` (shared hot files). | Surgical edits only (split-not-rewrite of `score_candidates_with_session`; additive config field; additive `deck_knowledge` fn; extract-not-rewrite of two `derived.rs` fns; additive `ai_gate.rs` arg). Re-read before edit. |

---

## 14. Files touched (summary)

| File | Change |
|---|---|
| `crates/phase-ai/src/determinize.rs` | **NEW** — `determinize_opponents`, `pinned_known_ids`, `unknown_slots`, `opponents_of`, `splitmix64` helper + tests A1–A7, D. Calls `engine::game::derived::sync_continuous_reveals`. |
| `crates/phase-ai/src/lib.rs` | `pub mod determinize;`. |
| `crates/phase-ai/src/deck_knowledge.rs` | Split `accounted_object_ids` → `public_account_object_ids` (+ preserve `accounted_object_ids`/`known_remaining_deck_counts` behavior); add `unknown_hidden_pool`. |
| `crates/phase-ai/src/search.rs` | Split `score_candidates_with_session` → `score_candidates_core(.., deadline_override)` + ensemble wrapper; `merge_into`/`finalize_mean` (with `debug_assert` observed==K) + tests B1–B4. |
| `crates/phase-ai/src/planner/mod.rs` | `PlannerServices::with_deadline` (or `deadline: Option<Deadline>` param) so the wrapper can share one wall-clock ceiling; `None` = today's derivation. |
| `crates/phase-ai/src/config.rs` | Add `determinization_samples: u32` **[F6]** to the search-config struct; set per preset (Medium 0, Hard 2, VeryHard/CEDH 3, WASM `min(2,tier)`, multiplayer capped). |
| `crates/engine/src/game/derived.rs` | **[F4]** Extract `sync_continuous_library_top_reveals` + `sync_continuous_hand_reveals` into one **public** `sync_continuous_reveals(&mut GameState)`; `derive_display_state` calls it (behavior-preserving). |
| `crates/phase-ai/src/bin/ai_gate.rs` | **[F1]** Add `--difficulty {medium|hard|veryhard|cedh}` to `parse_args` (`Args.difficulty: AiDifficulty`, default `Medium`); thread into `SuiteOptions::new(args.difficulty, ..)` at line 53. |

**Engine-crate edits are limited to the F4 `derived.rs` visibility-fn extraction** (making two private fns callable as one public fn) plus the existing call to `apply_card_face_to_object`. **No WASM export change. No frontend change. No engine visibility-rule change.**

---

## 15. CR annotations (grep-verified against `docs/MagicCompRules.txt`)

| CR | Verified text (grep) | Where annotated |
|---|---|---|
| **400.2** | "Library and hand are hidden zones, even if all the cards in one such zone happen to be revealed." | `determinize.rs` module doc + `unknown_hidden_pool` + `pinned_known_ids`; also the existing `derived.rs` reveal-sync annotation is preserved. |
| **401.2** | "Each library must be kept in a single face-down pile. Players can't look at or change the order of cards in a library." | shuffle/deal step (order randomization). |
| **401.3** | "Any player may count the number of cards remaining in any player's library at any time." | size-preservation assertions (hand + library len unchanged). |
| **402.1** | "The hand is where a player holds cards that have been drawn." (hand size countable ⇒ preserved) | `unknown_slots` (hand slot count preserved). |
| **701.20a** | "To reveal a card, show that card to all players for a brief time." | `pinned_known_ids` (revealed cards pinned). |
| **701.20e** | private-look visibility (the looker only) | `pinned_known_ids` private_look guard. |

All verified present in `docs/MagicCompRules.txt` during round-1 planning (400.2/401.2/401.3/402.1/701.20a at lines 1935, 1996, 1998, 2010, 3438); 701.20e in the same 701.20 block — the implementer re-greps each before writing the annotation per the CR protocol.
