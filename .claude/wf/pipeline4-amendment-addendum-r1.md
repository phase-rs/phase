# Pipeline 4 — Amendment ADDENDUM (r1): resolve the in-process-determinism contradiction

**Scope.** This is a NARROW addendum to the approved amendment plan
`pipeline4-amendment-plan-r3.md`. The amendment's implemented machinery is **FROZEN** and unchanged:
median-of-K=5 over independent cold child processes, the `1.05×+64` multiplicative band, the
`repro_margin_report` margin criterion (`PERF_REPRO_MARGIN_FRACTION = 0.5`, `PERF_REPRO_VALIDATION_RUNS = 25`),
schema version 2 + required `sample_count`, the `WorkloadMismatch` K-guard, the debug-primary CI profile, and the
3-way (`child` / `repro-report` / `parent`) binary dispatch. This addendum changes **only** five things:

1. **M9's assertion** (was: assert 18/19 in-process byte-equality — a false premise).
2. **The `perf.rs` module-doc paragraph** (currently states the false 18/19 in-process claim) + one stale const-doc timing figure.
3. **The M15 + committed-baseline VENUE and protocol** (the stale worktree engine is not the engine CI gates on).
4. **Budget-measurement sourcing** (fold the budget numbers out of the 25-run M15 rather than a separate measurement).
5. **The GH #4878 comment** (the in-process jitter finding materially extends the issue).

Everything else in r3 stands verbatim. No code beyond the M9 test body, the two doc regions, the three items above.

---

## Root-cause correction (the fact the whole addendum turns on)

r3 §0/§3.1h/GAP-2 asserted an **in-process invariant**: two sequential identical games in one process produce
byte-equal counters for 18 of 19 counters, with only `layers_full_eval` jittering, and `(winner, turn)` in-process
deterministic. **This premise is false.** The fix-round executor's M9 run (worktree engine ~c9a46a92, regenerated
card data, red-mirror, seed 2654435769, cap 3000, one in-process pair) found `(winner, turn)` equal but **six**
counters jittering in-process — `layers_full_eval` **plus five more**:

| counter | run 1 | run 2 | Δ |
|---|--:|--:|--:|
| state_clone_for_legality | 614 | 613 | −1 |
| static_full_scans | 5751 | 5745 | −6 |
| crew_eligibility_scans | 1164 | 1163 | −1 |
| legal_actions_spell_cost_sweeps | 31 | 30 | −1 |
| mana_aura_trigger_scans | 1857 | 1855 | −2 |
| layers_full_eval | (jitters — the one r3 expected) | | |

**Mechanism.** `std::collections::hash_map::RandomState::default()` seeds each `HashMap`/`HashSet` from a
thread-local `(k0, k1)` pair whose `k0` is **incremented by one on every allocation** (`RandomState::new()`).
Two sequential in-process games therefore allocate their maps at different counter offsets → different SipHash keys →
different iteration orders. **In-process repeat identity was never a std guarantee**; the original "18/19" reading was
a small-sample artifact (a single earlier pair that happened to land closer to byte-equal), with a possible additional
contribution from an engine-version difference in HashSet/HashMap usage between the measuring engines. Nothing in std
confines the iteration-order leak to *cross-process*: `state_clone_for_legality` diverging in-process (a
trajectory-coupled counter, per #4878) proves the game **line itself** — not just bookkeeping — can differ between two
in-process runs. Consequently **even `(winner, turn)` equality is empirical, not guaranteed.**

The frozen gate mechanism is unaffected by this correction: it never relied on in-process determinism. It compares the
**median over K independent cold processes** against a band-guarded baseline, and admits the baseline only after the
25-run cross-process margin validation. In-process determinism was only ever cited as supporting *evidence* in the doc
and asserted by M9 — both are corrected below.

---

## Decision 1 — M9 reframe: non-asserting in-process jitter DIAGNOSTIC with structural reach-guards

**Chosen option: (b), refined.** M9 becomes a `#[ignore]`, DB-gated **diagnostic** that drives two in-process games,
asserts only non-stochastic *structural* facts (so it can never flake), and **prints** the in-process pair-diff (which
feeds the #4878 comment, Decision 5). It asserts **no** stochastic equality — not `(winner, turn)`, not any counter
value — because per-allocation `RandomState` makes both stochastic in-process.

**Why not the other options.**
- *(a) assert `(winner, turn)` only* — rejected. `(winner, turn)` equality is empirical, not std-guaranteed; a future
  run whose in-process trajectory divergence reaches the winner/turn would fail M9 spuriously. Asserting a stochastic
  quantity is exactly the flake the amendment exists to eliminate.
- *(c) delete M9* — rejected. The in-process pair-diff is the cheapest evidence of #4878's in-process footprint and is
  the source for the Decision-5 comment. Keep it as a diagnostic; don't discard the observability.
- *(d) assert the margin criterion on small K* — rejected. That duplicates M15's job at a sample size too small to
  bound anything, and re-introduces a stochastic gate. M15 (25 runs) is the sole reproducibility *authority*.

**Operational placement / failure meaning.** M9 is `#[test] #[ignore = "..."]`, gated on `PHASE_CARDS_PATH`. It does
**not** run in the `test-ai` Tilt resource or in any CI job (neither runs `-- --ignored` for phase-ai). It is executed
**once, manually, by the executor** with `--ignored` on the baseline-generation venue (Decision 3), before the baseline
is committed. Its output is pasted into the PR and the #4878 comment. With the reframe it cannot fail spuriously; a
failure of its structural assertions means the harness did not drive two real games (a genuine regression in the test
harness or the perf-counter plumbing), not a determinism flake.

**Residual-flake detection (quantified).** Because M9 no longer asserts trajectory identity, the reproducibility risk
that matters is the **cross-process** drift the gate actually runs on. That risk is bounded — before every baseline
commit — by M15's 25-run margin validation: it records each counter's **worst** observed value across 25 independent
median-of-K runs and requires it at or below the 50%-of-headroom midpoint (a measured ≥2× safety factor). In steady CI,
median-of-K suppresses minority-outlier trajectories and the `1.05×+64` band absorbs the residual; a real regression
(target class ≥50% counter blow-up) still trips with ~10× margin. So the flake risk M9 used to (pretend to) guard is
detected and bounded by M15, not by an in-process equality assertion.

### Exact M9 replacement spec (test body, `crates/phase-ai/src/duel_suite/perf.rs`)

Rename the test to `perf_in_process_jitter_diagnostic` and rewrite the leading comment block (lines ~902–909) to state
the corrected premise (per-allocation `RandomState`; in-process identity is NOT guaranteed; the test observes and prints
the jitter footprint and asserts only structural liveness). Keep `#[ignore = "requires card database via PHASE_CARDS_PATH; run before baseline refresh"]`.
Keep the DB load, `find_matchup("red-mirror")`, `resolve_matchup`, and the two `reset()→drive_game(&payload, PERF_BASE_SEED, AiDifficulty::Medium, PERF_ACTION_CAP)→snapshot()` runs exactly as they are today (lines ~913–940). Replace the assertion tail (current lines ~942–970) with:

```rust
    let m1 = PerfCounters::from_snapshot(&snap_1).0;
    let m2 = PerfCounters::from_snapshot(&snap_2).0;

    // Structural reach-guards (NON-stochastic — cannot flake under #4878):
    //  1. real counters were recorded,
    //  2. both runs produced the identical, schema-total counter KEY SET
    //     (keys come from from_snapshot's total struct destructure, not the
    //     trajectory — deterministic regardless of HashSet iteration order),
    //  3. a core trajectory counter is non-zero, proving a real game ran.
    assert!(!m1.is_empty(), "counter map must be non-empty (real counters recorded)");
    assert_eq!(
        m1.keys().collect::<Vec<_>>(),
        m2.keys().collect::<Vec<_>>(),
        "counter key set is schema-total and identical across runs"
    );
    assert!(
        m1.get("state_clone_for_legality").copied().unwrap_or(0) > 0,
        "a real game must clone for legality at least once"
    );

    // DIAGNOSTIC (no assertion): print the in-process pair-diff. Under
    // per-allocation std RandomState (#4878) both (winner, turn) and every
    // HashSet-order-dependent counter may differ between two in-process runs;
    // this table documents the observed in-process jitter footprint and is
    // pasted into issue #4878. Equality is deliberately NOT asserted.
    println!("in-process (winner, turn): run1={wt_1:?} run2={wt_2:?} equal={}", wt_1 == wt_2);
    let mut jittered = 0usize;
    for (key, v1) in &m1 {
        let v2 = m2.get(key).copied().unwrap_or(0);
        if *v1 != v2 {
            jittered += 1;
            println!("  JITTER {key}: {v1} -> {v2} (delta {})", *v2 as i64 - *v1 as i64);
        }
    }
    println!("in-process jitter: {jittered} of {} counters differ (issue #4878)", m1.len());
```

Notes for the executor: `BTreeMap::keys()` yields sorted keys, so the `Vec` compare in guard (2) is order-stable and
correct. Do **not** re-add `wt_1 == wt_2` or any `assert_eq!(m1, m2)`-style value assertion. `wt_1`/`wt_2` remain bound
from the two `drive_game` calls (they are still used in the diagnostic `println!`).

---

## Decision 2 — Module-doc + const-doc correction (`crates/phase-ai/src/duel_suite/perf.rs`)

**(2a) Replace the false in-process sentence in the `//!` doc (current lines 23–26)** — the sentence beginning
"Within a single process the trajectory is deterministic: `(winner, turn)` and 18 of 19 counters are byte-equal…".
Replace it with a paragraph meeting **all** these content requirements (executor may phrase, but must include every
point; keep it within the existing doc's voice and the surrounding sentences intact):

- **What is stable:** *nothing* is guaranteed byte-stable across repeated runs — neither cross-process **nor
  in-process**. std `RandomState` seeds each `HashMap`/`HashSet` from a thread-local key pair bumped once per
  allocation, so even two sequential in-process games see different iteration orders. The game's macro trajectory
  (`(winner, turn)`) has been observed equal in the one measured in-process pair but is **not** guaranteed and must not
  be relied upon.
- **What jitters:** **any** HashSet/HashMap-iteration-order-dependent scan or clone count, **in- and cross-process** —
  observed in-process (worktree measurement): `layers_full_eval`, `state_clone_for_legality`, `static_full_scans`,
  `crew_eligibility_scans`, `legal_actions_spell_cost_sweeps`, `mana_aura_trigger_scans`; cross-process the divergence
  is larger and reaches trajectory-coupled counters. Attribute the mechanism as **per-allocation** `RandomState` (not
  "per-process", which wrongly implies in-process stability); cite issue #4878.
- **Why the gate still holds:** the gate never depends on any single-run or in-process determinism. It compares the
  **per-counter median over K independent cold-process samples** against the committed baseline under the `1.05×+64`
  band, and the baseline is admitted only after M15's 25-run margin validation bounds the worst observed drift at ≤50%
  of each counter's FAIL headroom (a measured ≥2× safety factor, not a formal false-positive bound). Median-of-K
  suppresses minority-outlier trajectories; the band absorbs residual drift; the margin validation empirically bounds
  it before commit.
- **Preserve** the existing surrounding statements that remain true: counter values are profile-independent; the
  authoritative gate profile is debug (`cargo ai-perf-gate`), run identically by CI and M15; when #4878 lands, K→1 and
  the band tightens to byte-exact.

Do **not** retain the words "18 of 19" or "only `layers_full_eval` jitters" anywhere in the doc.

**(2b) Correct the stale timing figure in the `PERF_SAMPLE_COUNT` const doc (line ~68).** The comment "K=5 keeps the
whole gate ~2 min" is contradicted by measurement (one K=5 gate run measured at 263 s ≈ 4.4 min, Decision 4). Change it
to reference the measured budget rather than an asserted figure, e.g. "K=5 keeps one gate run to a few minutes (M15
measured ≈ 4.4 min at the initial measurement; the committed budget is validated by
`scripts/validate-ai-perf-reproducibility.sh`, see plan §3.4), well under the 30-min CI timeout." Keep the #4878
K→1 note that follows.

**(2c) M9 comment block** (Decision 1) is the third doc region and is covered by the M9 spec above.

No other doc region changes. The binary's `//!` doc (`bin/ai_perf_gate.rs` lines 13–20) already states only the
cross-process claim and asserts no in-process determinism — leave it unchanged.

---

## Decision 3 — M15 + committed-baseline VENUE: current-main engine, not the stale worktree

**Ruling.** The committed baseline and its M15 validation **must** be generated on the **current-main engine** with
main-generated card data. The stale-worktree baseline is **rejected outright as a placeholder** — it is not a
placeholder, it is a broken gate: counter values are a function of the engine's decision logic (dominant) and the card
data. The worktree engine (~c9a46a92) makes materially different decisions (it cannot even deserialize main's
card-data — `ChoiceType::PermanentType` is missing), so a worktree baseline compared against a main-engine `current` in
CI would mismatch on the **first** CI run. A validation run on an engine other than the one CI gates is not a validation
at all.

### Venue protocol (exact steps for the executor)

1. **Fresh main worktree.** `git worktree add <path> origin/main` at the current `origin/main` tip. Do **not** use the
   stale `agent-ab012b2369009baa4` worktree for baseline generation.
2. **Port the committable amendment surface** onto the fresh-main worktree — cherry-pick the amendment commit(s) if
   they exist, else apply the diff for: `crates/phase-ai/src/duel_suite/perf.rs`,
   `crates/phase-ai/src/bin/ai_perf_gate.rs`, `scripts/ai-perf-gate.sh`, `scripts/refresh-ai-perf-baseline.sh`,
   `scripts/validate-ai-perf-reproducibility.sh`, and the `ai-gate.yml` perf-step comments. **Exclude** the stale
   worktree's regenerated `data/*` outputs, the `crates/phase-ai/baselines/perf-baseline.json` artifact, and the
   `crates/engine/data/oracle-subtypes.json` byproduct (per the executor's commit-surface note). This ports the gate
   onto the **main** engine.
3. **Regenerate card data on fresh-main with main's oracle-gen:**
   `cargo run --profile tool --features cli --bin oracle-gen -- data/ --stats --names-out data/card-names.json > data/card-data.json`.
   Confirm it deserializes (it will — same engine). Record the card count and the `git hash-object data/card-data.json`
   value for the PR (this is the baseline's provenance `card_data_hash`). Note: exact MTGJSON-version parity with CI's
   regen is **not** required for correctness — `card_data_hash` is provenance-only/non-gating and the band absorbs
   card-data drift — but generating on main-oracle-gen minimizes first-run false FAILs.
4. **M9 diagnostic:** run the reframed M9 with `--ignored` on fresh-main; paste its in-process jitter table into the PR
   and into the #4878 comment (Decision 5).
5. **M15 on fresh-main:** run `scripts/validate-ai-perf-reproducibility.sh` (unchanged from r3 §3.3). It builds the
   debug binary, generates the median-of-K baseline, runs `PERF_REPRO_VALIDATION_RUNS = 25` further median-of-K gate
   runs, runs the margin gate, and prints the budget numbers (Decision 4).
6. **Commit gate.** Commit `perf-baseline.json` **only if, on this fresh-main venue,** (i) the margin gate passes
   (`all_within_margin()`, exit 0) **and** all 25 band-gate runs exit 0, **and** (ii) the measured budget check passes
   (Decision 4). The margin validation MUST pass on the venue that generates the committed baseline.
7. **Land ordering.** Commit the baseline JSON together with (or in the same PR as) the gate code, so main never has
   the two perf CI jobs active without a matching baseline. If for any reason the gate code lands before the baseline,
   the two perf jobs must be non-blocking (not required) until the baseline commit lands — a perf job with no baseline
   exits 2 and would red-wall the queue.

### If a counter is OVER-MARGIN on the fresh-main venue (pre-authorized escalation ladder)

The executor may apply the following **without a new planning round**, in order, re-running M15 after each step:

- **Lever 1 (primary): raise `PERF_SAMPLE_COUNT`.** 5 → 7 → 9 (must stay **odd**). Higher K shrinks the median's
  run-to-run spread and is the principled fix for a margin miss. **Cap: K ≤ 9, and only if the budget check
  (Decision 4) still passes at the raised K** — each +2 adds ~2 sequential child processes (≈ +2·W_debug per run).
  If raising K to fix the margin breaks the option-(c) budget, that is itself the trigger for the option-(b) release
  fallback (r3 §3.4), not a reason to exceed K=9.
- **Lever 2 (secondary, only if K=9 still has an OVER-MARGIN counter): widen the global band**, with hard caps:
  `PERF_TOLERANCE_RATIO` ≤ **1.10** and/or `PERF_ABSOLUTE_FLOOR` ≤ **128**. These caps are chosen so the gate still
  catches the target class: structural regressions are ≥50% counter blow-ups, so even at 1.10× the gate trips with ~5×
  margin. Widen only the parameter that covers the offending counter (floor for small-count counters, ratio for
  large-count counters), document the OVER-MARGIN row(s) and the new value with a rationale comment on the const, and
  re-run M15.
- **Beyond the caps → escalate to a new planning round.** A band looser than 1.10×/128, or a K above 9, or a margin
  miss that neither lever resolves, is out of pre-authorized bounds: do NOT commit; return to planning. (Never raise
  `timeout-minutes` and never demote the offending counter.)

---

## Decision 4 — Budget acceptance: the 25-run M15 IS the budget measurement

**Ruling.** Yes — the 25-run M15 validation (Decision 3, step 5) **doubles as the budget measurement**. No separate
budget run is needed; every budget number is extracted from the M15 output on the fresh-main venue:

| Quantity | Source (from the M15 run) |
|---|---|
| `T_run_max` (one CI perf-job execution = K children + compare) | **max** over the 25 `run i wall=<s>s` lines the validation script prints (these are `--current-output` compare runs — the exact mode CI runs `cargo ai-perf-gate` in, so more representative than the executor's single `--refresh-baseline` run) |
| `W_debug` (per-sample debug wall) | `W_debug = T_run_max / PERF_SAMPLE_COUNT` (children run sequentially via blocking `.status()`) |
| `T_build` (debug build ceiling) | the script's printed `time` of its **cold-isolated** `cargo build --bin ai-perf-gate` (real seconds) |

**Acceptance arithmetic (option c, computed by the executor from the measured numbers):**

```
T_run_max × PERF_CI_RUNNER_MARGIN(2.5)  +  T_build  <  PERF_CI_BUDGET_CEILING_MIN(25 min = 1500 s)
```

- `T_build` is measured **cold-isolated**, which is ≥ CI's warm-cache build (the `rust-ai-gate` shared key is
  debug-warm from the win-rate jobs). GH #4878's reviewer accepted `T_build` as warm-cache-scoped, so using the cold
  figure is **conservative**: a pass with cold `T_build` is safe because CI's real build is ≤ it.
- `T_run_max` (the **max**, not median, of the 25 runs) is the tail-run CI would pay; multiplied by the explicit 2.5
  runner-slowdown factor for 2-core `ubuntu-latest`.
- The card-data restore/regen step is separately keyed and already proven to fit 30 min by the green win-rate job
  (r3 §3.4) — note it, don't re-measure.
- **Pass → commit** (with the Decision-3 correctness gates). **Fail → option-(b) fallback** (r3 §3.4): switch the two
  perf `run:` lines to `cargo run --release …` + a dedicated `rust-ai-perf-release` cache key, re-measure in release
  (`cold_release_build` via `rm -rf target/ai && time scripts/ai-perf-gate.sh`, and `T_run_release_max` over 25 release
  runs), and commit only if `cold_release_build + T_run_release_max×2.5 + T_build_release < 25 min`.

**Provisional expectation (NOT a substitute for the measured M15 numbers).** From the executor's single worktree run
(263 s at K=5): 263×2.5 ≈ 11.0 min for the gate step; even a generous cold `T_build` ≈ 10 min totals ≈ 21 min < 25 min,
so option (c) is very likely to pass — but the executor **must** confirm with the actual 25-run `T_run_max` and cold
`T_build` on fresh-main, because `T_run_max` is a tail value that can exceed the single observed 263 s.

`timeout-minutes` stays at 30 on both perf jobs in every branch.

---

## Decision 5 — GH #4878 comment (content requirements)

Add a comment to issue #4878 (the executor posts it after the fresh-main M9 diagnostic run). The comment framing:
"in-process footprint of the iteration-order leak — extends this issue beyond the cross-process axis it was filed on."
It must contain:

1. **Per-allocation `RandomState` mechanism.** `std` `RandomState::default()` seeds each `HashMap`/`HashSet` from a
   thread-local `(k0, k1)` key pair whose `k0` is incremented by one on every `RandomState::new()`. Sequential map
   allocations therefore get different SipHash keys, and two sequential **in-process** games allocate their maps at
   different counter offsets → different iteration orders. So the iteration-order leak is **not** confined to
   cross-process — it also varies in-process between repeated runs. The issue's original "within one process: 18/19
   byte-identical" reading was a small-sample artifact, not a std guarantee.
2. **The in-process jitter table** (state it was one in-process pair; worktree engine ~c9a46a92, regenerated card data,
   red-mirror, seed 2654435769, cap 3000; `(winner, turn)` equal in this pair): the five-counter table above plus
   `layers_full_eval`. Call out that `state_clone_for_legality` diverging in-process confirms the **game line itself**
   (not just bookkeeping) can differ within a single process — the same trajectory-coupling the issue already
   identified cross-process, now observed in-process.
3. **Engine-version caveat.** Measured on a stale worktree engine (~c9a46a92) with card data regenerated by an older
   oracle-gen, not current main; an engine-version difference in HashSet/HashMap usage between that engine and the one
   the original within-process measurement used cannot be excluded as a contributing factor. The qualitative
   conclusion (per-allocation `RandomState` ⇒ in-process iteration-order variance ⇒ trajectory-coupled counters can
   jitter in-process) holds regardless of engine version; only the exact deltas are engine/card-data-specific.
4. **Harness implication.** The perf-gate regression harness must **not** assume in-process determinism: the gate now
   uses median-of-K over independent cold processes + a 25-run margin validation rather than any in-process identity
   assertion (M9 is now a non-asserting diagnostic). The fix scope for #4878 itself is unchanged (replace
   order-leaking `HashSet`/`HashMap` with deterministic-order structures at the leak sites; CR 613 layers path is
   sensitive — don't regress #4620).

No CR annotations are involved (AI-infra); no CR verification required.

---

## Executor checklist (this addendum only)

- [ ] Rewrite M9 body + comment per Decision 1 (rename `perf_in_process_jitter_diagnostic`; structural reach-guards +
      printed pair-diff; no stochastic assertions). Keep `#[ignore]`, DB-gated.
- [ ] Correct the `//!` in-process paragraph (2a) and the `PERF_SAMPLE_COUNT` "~2 min" const doc (2b).
- [ ] Generate the baseline + run M15 on a **fresh origin/main worktree** with the ported gate code and main-oracle-gen
      card data (Decision 3). Run reframed M9 `--ignored` there; paste its table into the PR + #4878.
- [ ] Extract `T_run_max` / `W_debug` / `T_build` from the M15 run; apply the option-(c) budget arithmetic
      (Decision 4); fall back to option (b) only if it fails.
- [ ] Commit `perf-baseline.json` only if margin gate + 25 band runs + budget all pass on fresh-main; land it with the
      gate code (or keep the perf jobs non-required until it lands).
- [ ] Post the #4878 comment (Decision 5).
- [ ] `cargo fmt --all`; verify phase-ai compiles + `duel_suite::perf` unit tests green (Tilt `test-ai`/`clippy` if up,
      else targeted cargo with an isolated `CARGO_TARGET_DIR`).

**Frozen (do not touch):** median-of-K=5, `1.05×+64` band, `PERF_REPRO_MARGIN_FRACTION = 0.5`,
`PERF_REPRO_VALIDATION_RUNS = 25`, schema 2 + `sample_count`, the `WorkloadMismatch` K-guard, `median_report` /
`repro_margin_report` / `print_repro_margin`, the 3-way binary dispatch, debug-primary CI, and the M11–M14/M-even/
M-margin tests.
