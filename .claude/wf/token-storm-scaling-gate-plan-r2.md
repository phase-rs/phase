# Token-Storm Scaling CI Gate — Implementation Plan (Round 2)

## Amendments applied (round 2)

Each amendment was re-verified against live code before folding into the plan below.

**A1 — Seed-static snippet now compiles (MEDIUM, fixed).** Verified `StaticDefinition::affected(mut self, filter: TargetFilter)` at `types/ability.rs:16565` — it takes `TargetFilter`, **not** `TypedFilter`, so the round-1 snippet would not have compiled. `StaticDefinition::new(mode: StaticMode)` is at `:16545`. The proven bare-global idiom is confirmed at **three** live sites, all identical: `targeting.rs:4748`, `game_state.rs:9136`, and the default-test at `game_state.rs:9130-9136` — each does `obj.static_definitions = vec![StaticDefinition::new(StaticMode::X)].into();` with **no `.affected()`**, which sets the presence bit for a board-global keyword grant. Corrected seed snippet uses `StaticMode::Vigilance` in exactly this form (see Step 1a). The field is `static_definitions` and the RHS is `.into()` (from `Vec<StaticDefinition>`).

**A2 — Class-b claim scoped to the FULL-FLUSH seam (LOW/MED, fixed).** Verified two `refresh_static_mode_presence` call sites in `layers.rs`: line **1920** (inside `evaluate_layers`, the full-derivation path) and line **1983** (inside `flush_layers`'s `EnteredObjects` incremental arm). `flush_layers` (`:1963`) dispatches `LayersDirty::Full => evaluate_layers(state)` at `:1968`, so a restored state (`layers_dirty` defaults to `LayersDirty::full()`) drives the **Full** arm → `:1920`. This gate therefore covers the full-flush/AI-restore class specifically — the class that caused the original bug. The incremental (`EnteredObjects`, `:1983`) arm is already covered by Unit 1's existing tests, verified present at `layers.rs:15858` under the literal section header `// ── StaticModePresence refresh tests (Verification Matrix D/F) ──`:
  - **Test D** = `incremental_flush_refreshes_static_mode_presence` (`layers.rs:15865`) — drives `LayersDirty::EnteredObjects([entrant])` → `flush_layers`, asserts `layers_incremental == 1` (not escalated) and that the presence index is refreshed precisely (revert guard on the `:1983` call).
  - **Test F** = `static_mode_presence_equals_functioning_statics_fold` (`layers.rs:15926`) — building-block equivalence for every kind.

  No new `EnteredObjects` test is added here — it would duplicate Test D with no new signal.

**A3 — 1b restated + cheap combat direct-drive added (LOW, fixed after body inspection).** I inspected both enumerator **bodies** (not just signatures):
  - `get_valid_attacker_ids(state: &GameState) -> Vec<ObjectId>` (`combat.rs:2918`). Body calls `CombatStaticGates::compute(state)` (`:2922`), whose fields are all `static_kind_present(state, StaticModeKind::…)` reads (`combat.rs:56-65`) — the O(1) presence index. Inside the per-creature `.filter_map`, when a gate bit is set it calls `check_static_ability(...)` (`static_abilities.rs:676`), which records a full scan (`record_static_full_scan()` at `static_abilities.rs:689`). **The parameter is `&GameState` (immutable) → the function cannot call `flush_layers`; it does not self-heal.** On an unflushed restored state (`static_mode_presence` = all-present serde default), every gate bit reads true, so each active-player creature triggers per-creature `check_static_ability` full scans → explosion. On a flushed state the gates are precise-absent → the `&&` chain short-circuits → zero scans. **The direct-drive check is sound.**
  - `get_valid_block_targets_for_player(state: &GameState, player)` (`combat.rs:3369`) delegates to `get_valid_block_targets(state)`, which gates the same way (`static_kind_present` at `combat.rs:672-675`, etc.) and is likewise `&GameState`. Same non-self-healing property.

  **Two corrections carried into the plan:** (a) the real signature is single-arg `get_valid_attacker_ids(&state)` — it reads `state.active_player` internally, **not** `(&s, player)` as the amendment text drafted it; (b) the per-creature scan branch only executes for creatures where `obj.controller == active_player` (the first conjunct at `combat.rs:2929`). A non-vacuous assertion therefore requires the 1000 tokens to be controlled by `state.active_player`. The plan reads `let active = state.active_player;` and builds tokens under `active`. 1b is restated as class-a composite only; the direct-drive class-b check lives in 1c.

**A4 — Overlap and substitution acknowledged (LOW, fixed).** Verified `token_storm_target_enumeration_does_no_static_full_scans` at `targeting.rs:4683-4721`: it hand-builds 1000 tokens, calls `evaluate_layers` (`:4706`), then `find_legal_targets` + asserts `static_full_scans == 0` and `targets.len() == 1000`. This **already covers 1a's class-a signal on a hand-built board.** 1a's unique value is the production-shaped **serde round-trip restore + `flush_layers`** path (class b). Deliberate substitution documented: the unit test refreshes via `evaluate_layers` (→ `refresh_static_mode_presence` at `layers.rs:1920`); 1a refreshes via `flush_layers` (`layers.rs:1963`, whose `Full` arm calls `evaluate_layers` at `:1968` → same `:1920` refresh). Both leave a precise index; 1a additionally exercises the `flush_layers` dispatch + `mark_public_state_all_dirty` seam that production hits.

**A5 — DB-free risk note retained (LOW, fixed).** Verified `scripts/check-test-card-data-load.sh`: it is a diff-based gate (`set -euo pipefail`, base = `git merge-base origin/main HEAD`) that flags newly-added test lines loading the full ~90 MB `client/public/card-data.json`. Its header confirms the operative fact for exact-0 assertions: "**Under `cargo nextest` every test runs in its own process**" — so `thread_local!` perf counters (`perf_counters.rs:35`) cannot bleed across tests, making `== 0` safe. Both files must mirror the DB-free idiom of the `targeting.rs:4683` unit test (`GameState::new_two_player` + `create_object`, zero card-DB load), and this script is the guard that catches a regression.

---

## Task

Add a permanent CI regression gate proving the `StaticModePresence` O(1) index keeps whole-battlefield static scans out of the two hot enumeration paths (target legality, combat declaration) at scale, **including the production serde-restore → `flush_layers` seam** that the AI search and saved-state loader hit. This locks in the Unit 1/Unit 2 perf work (commits `a7ea537`, `dd8fe5d`, `48861f8`) against silent regression.

## Applicable skills

No new engine surface is introduced (no new effect, keyword, trigger, static, variant, or parser pattern). This is a **test-only** change composing existing pub APIs, so `/add-engine-effect`, `/add-engine-variant`, and the parser skills do not apply. The governing guidance is `/card-test`'s foot-gun list (adapted: these are enumeration/perf tests, not cast-pipeline tests) and the CLAUDE.md verification-matrix contract. No `mtgish` involvement.

## Analogous trace (hard gate)

Traced the existing perf-gate feature end-to-end:
`token_storm_target_enumeration_does_no_static_full_scans` (`targeting.rs:4683`) →
`find_legal_targets` (`targeting.rs:16`, non-self-healing) →
`can_target` hexproof gate → `static_kind_present` (`game/functioning_abilities.rs`) → `state.static_mode_presence.contains(StaticModeKind::IgnoreHexproof)` →
index built by `refresh_static_mode_presence` (`layers.rs:2299`, called from `evaluate_layers` `:1920` and `flush_layers` `:1983`) →
scan counter `record_static_full_scan` (`static_abilities.rs:689` et al.) → `perf_counters` `thread_local` (`:35`), `reset`/`snapshot` pub (`:229`/`:233`).
Combat sibling: `squirrel_perf_probe` DeclareAttackers idiom (`combat.rs:~100` region test) → `get_valid_attacker_ids` (`combat.rs:2918`) → `CombatStaticGates::compute` (`:56`) → `static_kind_present`.

## Pattern coverage

Covers the **class of all whole-battlefield static-scan gates** funnelled through `static_mode_presence` — 36+ scan sites migrated in Unit 1/2 across `targeting.rs`, `combat.rs`, `static_abilities.rs`. The two probes (target enumeration, combat declaration) are the two highest-fanout production entry points; the serde-restore variant covers every AI-search / saved-game consumer that deserializes a `GameState` and relies on `#[serde(skip, default = all_present)]` + a subsequent flush. Not one card — the whole indexed-gate architecture.

## Building blocks (existing only — no new helpers)

- `GameState::new_two_player(u64)` (`game_state.rs:8118`); `create_object` (`zones.rs:475`).
- `layers::flush_layers` (`layers.rs:1963`), `layers::evaluate_layers` (`:1570`); `LayersDirty::full()`.
- `perf_counters::reset` / `snapshot` (`perf_counters.rs:229`/`:233`); fields `static_full_scans`, `combat_shadow_block_scans`.
- `find_legal_targets` (`targeting.rs:16`); `get_valid_attacker_ids` (`combat.rs:2918`), `get_valid_attack_targets`, `get_valid_block_targets_for_player` (`combat.rs:3369`).
- `StaticDefinition::new` (`ability.rs:16545`); `StaticMode::Vigilance`; `StaticModePresence::contains` (`statics.rs:2114`), `StaticModeKind`.
- phase-ai: `load_saved_game_state` (`saved_state.rs:21`, flushes on load); `search::choose_action(&GameState, …)` (`search.rs:110`); `create_config_for_players` (`config.rs:976`), `AiDifficulty::VeryEasy`, `Platform::Native` (`config.rs:79-82`), all re-exported at `lib.rs:36-38`.

No new helper is justified — every primitive exists.

## Logic placement

100% test code. FILE 1 (engine tier) lives at `crates/engine/tests/token_storm_scaling_gate.rs`, runs under Tilt `test-engine` (`cargo nextest run -p engine`, Tiltfile:123). FILE 2 (phase-ai tier) is a **new** file at `crates/phase-ai/tests/token_storm_restore_flush_gate.rs`, runs under Tilt `test-ai` (`-p phase-ai`, Tiltfile:132). File 2 must be new-only because concurrent uncommitted phase-ai work exists (`src/policies/*`, `config.rs`) — no edits to existing phase-ai files.

## Rust idioms

`const TOKENS: usize = 1000`; iterator `.push(CoreType::Creature)` mutation mirroring the `4683` idiom; typed `StaticModeKind`/`StaticMode` enums (no strings, no bools); `assert_eq!(counters.static_full_scans, 0, …)` for absent-mode exactness and loose `<` bounds for legitimate O(N) work. Read `state.active_player` rather than hard-coding a seat.

## Extension vs creation

Extends the existing `token_storm_*` / `squirrel_perf_probe` perf-gate pattern to (a) 1000-object scale and (b) the serde-restore production seam. No new pattern.

---

## Implementation steps

### FILE 1 — `crates/engine/tests/token_storm_scaling_gate.rs` (new)

**Shared helpers (module-top, DB-free — A5):**

```rust
/// 1000 vanilla creature tokens controlled by `owner`, plus one bare global
/// Vigilance static so the presence index is provably non-empty yet does NOT
/// touch the IgnoreHexproof bit the target-enumeration gate consults.
fn token_storm_board(owner: PlayerId) -> (GameState, Vec<ObjectId>) {
    let mut state = GameState::new_two_player(42);
    const TOKENS: usize = 1000;
    let mut ids = Vec::with_capacity(TOKENS);
    for i in 0..TOKENS {
        let id = create_object(&mut state, CardId(1000 + i as u64), owner,
            format!("Token{i}"), Zone::Battlefield);
        state.objects.get_mut(&id).unwrap()
            .card_types.core_types.push(CoreType::Creature);
        ids.push(id);
    }
    // A1 corrected seed static — bare global keyword grant, NO `.affected()`.
    // Mirrors targeting.rs:4748 / game_state.rs:9136. Sets StaticModeKind::Vigilance
    // only; leaves IgnoreHexproof precisely absent after a flush.
    let src = create_object(&mut state, CardId(9999), owner,
        "Vigilance Anthem".to_string(), Zone::Battlefield);
    state.objects.get_mut(&src).unwrap().static_definitions =
        vec![StaticDefinition::new(StaticMode::Vigilance)].into();
    (state, ids)
}

/// Serde round-trip: drops every `#[serde(skip)]` field, so the restored state
/// has `static_mode_presence = all_present` and `layers_dirty = LayersDirty::full()`
/// — exactly the production shape an AI-search / saved-game restore produces.
fn restore(state: &GameState) -> GameState {
    serde_json::from_str(&serde_json::to_string(state).unwrap()).unwrap()
}
```

**Test 1a — `token_storm_scaling_gate_absent_modes` (class a + class b, FULL seam):**
1. `let (state, ids) = token_storm_board(PlayerId(1));`
2. `let mut restored = restore(&state);` — index all-present, `layers_dirty = Full`.
3. `layers::flush_layers(&mut restored);` — **Full arm** → `evaluate_layers` → `refresh_static_mode_presence` (`:1920`) → precise index (IgnoreHexproof absent, Vigilance present).
4. `perf_counters::reset();`
5. `let targets = find_legal_targets(&restored, &creature_filter, PlayerId(0), ObjectId(99));`
6. Assert `snapshot().static_full_scans == 0` — **class a** revert guard (removing the `can_target` presence gate → non-zero) **and class b** (removing the `flush_layers` call leaves all-present → non-zero).
7. Assert `targets.len() == 1000`; assert membership of `ids[0]` and `ids[999]` (correctness anchor so the 0-scan isn't from an empty result).

Overlap note (A4) in a doc comment: class-a signal duplicates `targeting.rs:4683`; this test's unique contribution is the serde-restore + `flush_layers` (Full → `evaluate_layers` → `:1920`) production seam.

**Test 1b — `token_storm_declare_attackers_gate` (class-a composite only — A3):**
1. `token_storm_board`, `restore`, `flush_layers` (as 1a).
2. Build `valid_attacker_ids = get_valid_attacker_ids(&restored)` and `valid_attack_targets = get_valid_attack_targets(&restored)`; set `restored.waiting_for = WaitingFor::DeclareAttackers { … }` from them (mirror `combat.rs:2996-3006`).
3. `perf_counters::reset();` → `let actions = legal_actions_full(&restored);`
4. Assert `combat_shadow_block_scans == 0 && static_full_scans < 20_000` (loose bound — legitimate per-attacker O(N) work) and `actions.len() >= 1`.
5. **Doc comment (A3):** `legal_actions_full` self-heals (`ai_support/mod.rs:948-951`, `:960-966`), so this asserts the class-a index-precision composite only. The combat enumerators used to *build* the state carry no class-b guard here — that guard is Test 1c.

**Test 1c — `token_storm_missing_flush_explodes` + combat direct-drive (class-b positive control — A3):**
1. `let (state, _) = token_storm_board(PlayerId(1)); let unflushed = restore(&state);` — **SKIP flush** (index stays all-present).
2. Target path: `perf_counters::reset(); find_legal_targets(&unflushed, &creature_filter, PlayerId(0), ObjectId(99));` → assert `snapshot().static_full_scans >= 1000`. Pins the mechanism (all-present index → every token forces a scan) and guards 1a against a future self-heal of `find_legal_targets` making its 0 vacuous.
3. **Combat direct-drive (new, A3):** build a fresh board whose tokens are controlled by the active player so the per-creature scan branch (`combat.rs:2929`, `obj.controller == active`) is actually reached:
   ```rust
   // Read active first, then build tokens under it so combat.rs:2929 passes.
   let probe = GameState::new_two_player(42);
   let active = probe.active_player;
   let (state, _) = token_storm_board(active);
   let unflushed = restore(&state);
   perf_counters::reset();
   let _ = combat::get_valid_attacker_ids(&unflushed); // &GameState, cannot self-heal
   assert!(perf_counters::snapshot().static_full_scans >= 1000,
       "unflushed all-present index forces a per-creature combat static scan");
   ```
   (Single-arg signature confirmed `combat.rs:2918`; reads `state.active_player` internally.) This is the class-b guard the combat path lacks in 1b: on the flushed state the same call yields 0 scans, on the unflushed state it explodes — precisely the regression the index prevents.

### FILE 2 — `crates/phase-ai/tests/token_storm_restore_flush_gate.rs` (NEW file only)

Local board builder using engine pub API (same DB-free idiom); wrap in the saved-state envelope `{"gameState": <state-json>}`.

**Test 2a — `load_saved_game_state_flushes_presence_index` (always-on backbone):**
1. Build 1000-token board (owner `PlayerId(1)`) + Vigilance seed; serialize into the `{"gameState": …}` envelope.
2. `let loaded = load_saved_game_state(&envelope)?;` (`saved_state.rs:21` flushes on load).
3. Assert `!loaded.static_mode_presence.contains(StaticModeKind::IgnoreHexproof)` — trips if the `saved_state.rs:21` flush is deleted (index would stay all-present).
4. `perf_counters::reset(); find_legal_targets(&loaded, &creature_filter, PlayerId(0), ObjectId(99));` → assert `static_full_scans == 0` and `targets.len() == 1000`.

**Test 2b — `choose_action_on_restored_board_is_bounded` (`#[ignore]` defense-in-depth):**
1. `#[ignore = "opt-in perf backstop; 2a is the always-on gate"]`.
2. Load restored board; `let cfg = create_config_for_players(2, AiDifficulty::VeryEasy, Platform::Native);` seeded `SmallRng`.
3. `perf_counters::reset(); let _ = choose_action(&loaded, &cfg, &mut rng, …);` (`search.rs:110`).
4. Assert `static_full_scans < 200_000` (loose search-depth bound).
5. In-file doc comment: `perf_counters` is `thread_local`; nextest process-per-test isolates it, but `choose_action` may spawn work — the bound is deliberately loose and the test is `#[ignore]`.

---

## Verification matrix

| Claim | Changed seam / entry point | Test | Revert-failing assertion | Class |
|---|---|---|---|---|
| Target enumeration does no full scan at scale after full-flush restore | `can_target` presence gate; `flush_layers` Full arm (`layers.rs:1968→1920`) | 1a | `static_full_scans == 0` (+ `len == 1000`) | a **and** b (full-flush) |
| Combat declaration bounded after restore | `CombatStaticGates` presence gates; `legal_actions_full` self-heal | 1b | `combat_shadow_block_scans == 0 && static_full_scans < 20_000` | a only |
| Missing flush ⇒ explosion (mechanism pin) — target path | all-present serde default | 1c | `static_full_scans >= 1000` | b (positive control) |
| Combat enumerator carries class-b guard (non-self-healing `&GameState`) | `get_valid_attacker_ids` (`combat.rs:2918`) | 1c direct-drive | `static_full_scans >= 1000` unflushed | b (positive control) |
| `load_saved_game_state` flushes the index | `saved_state.rs:21` | 2a | `!contains(IgnoreHexproof)` + `static_full_scans == 0` | b (saved-state) |
| Full AI search bounded on restored board | `choose_action` (`search.rs:110`) | 2b (`#[ignore]`) | `static_full_scans < 200_000` | b (defense-in-depth) |
| Incremental (`EnteredObjects`) refresh precise | `flush_layers` incremental arm (`layers.rs:1983`) | **Existing** Test D `incremental_flush_refreshes_static_mode_presence` (`layers.rs:15865`); Test F `static_mode_presence_equals_functioning_statics_fold` (`:15926`) | already revert-guarded (`layers_incremental==1`, precise presence) | b (incremental) — covered by Unit 1 |

**Hostile / negative reach-guards:** 1a's `len == 1000` + member checks prevent a vacuous 0-scan from an empty result; 1c is the paired positive control that proves the 0-scan assertions are reachable (not short-circuited); Vigilance-present-while-IgnoreHexproof-absent proves the index discriminates kinds (not "all-empty"); combat direct-drive builds tokens under `active_player` so `combat.rs:2929` is reached (not vacuously short-circuited on controller mismatch).

## Risks

- **DB-free (A5):** neither file may load `client/public/card-data.json`; both use `new_two_player` + `create_object` only. `scripts/check-test-card-data-load.sh` (diff-based, `set -euo pipefail`, merge-base vs origin/main) is the guard and confirms nextest's process-per-test isolation that makes `== 0` sound.
- **Combat controller subtlety (A3):** tokens must be controlled by `state.active_player` or the combat scan branch never runs and the assertion is vacuous — handled by reading `active` and building under it.
- **phase-ai concurrency:** File 2 is new-only; no edits to `src/policies/*` or `config.rs`.

## Verification cadence

`cargo fmt --all` (always direct). Then, with Tilt up (`tilt get uiresource clippy >/dev/null 2>&1`): `./scripts/tilt-wait.sh test-engine test-ai` and read `tilt logs test-engine`/`test-ai`. During implementation only (never committed): temporarily flip the `saved_state.rs:21` flush and one Unit-2 presence gate to confirm 2a and 1a/1c go red, record the red output, then revert. Do not run `cargo test`/`clippy` directly (target-lock contention).
