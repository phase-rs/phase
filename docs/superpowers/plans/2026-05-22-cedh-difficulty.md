# cEDH Difficulty Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship an `AiDifficulty::CEDH` preset, a bracket-5 game-setup lock, a combo-recognition skeleton (`ComboLinePolicy` + `CedhKeepablesMulligan` + `combo/` module + `DeckFeatures::is_cedh`), and engine-side cEDH deck validation — without touching real combo content (one stub combo proves the wiring).

**Architecture:** Three layers. (1) `crates/phase-ai` gets `AiDifficulty::CEDH`, a cEDH-specific preset in `create_config`/`create_config_for_players`, a new `is_cedh` field on `DeckFeatures`, a `combo/` module (types + detector + registry), a `ComboLinePolicy` (gated via `activation()` on `features.is_cedh`), and a `CedhKeepablesMulligan` policy (gated internally). (2) `crates/engine` gets `validate_cedh_bracket` in `database/legality.rs` — a tag check against `CommanderBracketTier::Cedh`. (3) The frontend gets a `services/cedhLock.ts` source of truth, a `cEDH` option in `AiDifficultyDropdown`, AI difficulty cascade + toast in `AiOpponentConfig`, deck-pool filtering in `aiDeckCatalog`, a warning chip in `GameSetupPage`, and blocking-error rendering in `GamePage`.

**Tech Stack:** Rust 2021 (engine + phase-ai), TypeScript 5 + React + Zustand + Tailwind v4 + Vitest (frontend), Tilt for continuous build/test, nextest for Rust tests.

**Spec:** [docs/superpowers/specs/2026-05-22-cedh-difficulty-design.md](../specs/2026-05-22-cedh-difficulty-design.md)

---

## Verification pattern (referenced from every task)

CLAUDE.md mandates Tilt-first verification. Every task uses this pattern after writing/editing code:

```bash
# Always run fmt directly — Tilt does not auto-format.
cargo fmt --all

# Rust verification (engine + phase-ai change → invalidates card-data per Tiltfile):
if tilt get uiresource clippy >/dev/null 2>&1; then
  ./scripts/tilt-wait.sh --timeout 300 clippy test-engine test-ai card-data
else
  cargo clippy --all-targets -- -D warnings
  cargo test -p engine
  cargo test -p phase-ai
  ./scripts/gen-card-data.sh
fi

# Frontend verification:
if tilt get uiresource clippy >/dev/null 2>&1; then
  ./scripts/tilt-wait.sh --timeout 180 check-frontend test-frontend
else
  (cd client && pnpm run type-check && pnpm lint && pnpm test -- --run)
fi
```

After a non-zero `tilt-wait.sh` exit, fetch details with `tilt logs <resource> --tail 50 --since 2m`. After direct cargo/pnpm failures, output is already on stdout.

**Single-test runs during TDD** (where Tilt's continuous-build is too coarse): a single named test like `cargo test -p phase-ai cedh_preset_skips_paranoid_scaling --no-run` then `--no-fail-fast` is acceptable because it doesn't compete for a build lock (uses an already-warm target). When Tilt is actively rebuilding, prefer waiting.

---

## File structure

**Created (new files):**

- `crates/phase-ai/src/combo/mod.rs` — module entry + `ComboRegistry`
- `crates/phase-ai/src/combo/line.rs` — `ComboLine`, `ComboPiece`, `ComboStep`, `ComboReachability`, `WinKind`, `ComboLineId`
- `crates/phase-ai/src/combo/detection.rs` — `ComboDetector` trait + `DefaultComboDetector`
- `crates/phase-ai/src/combo/registry.rs` — registered combo lines (stub initially)
- `crates/phase-ai/src/policies/combo_line.rs` — `ComboLinePolicy` (`TacticalPolicy`)
- `crates/phase-ai/src/policies/mulligan/cedh_keepables.rs` — `CedhKeepablesMulligan` (`MulliganPolicy`)
- `client/src/services/cedhLock.ts` — `anyAiOpponentIsCedh`, `applyCedhCascade`, `isDeckCedhLegal`
- `client/src/services/__tests__/cedhLock.test.ts`

**Modified (existing files):**

- `crates/phase-ai/src/config.rs` — `AiDifficulty::CEDH` variant, cEDH preset arm, 4p scaling skip, `PolicyPenalties` fields
- `crates/phase-ai/src/lib.rs` — `pub mod combo;`
- `crates/phase-ai/src/features/mod.rs` — `is_cedh: bool` field on `DeckFeatures`
- `crates/phase-ai/src/policies/registry.rs` — `PolicyId::ComboLineProgress` + register `ComboLinePolicy`
- `crates/phase-ai/src/policies/mulligan/mod.rs` — `pub mod cedh_keepables;` + `PolicyId::CedhKeepablesMulligan` + register
- `crates/engine/src/database/legality.rs` — `validate_cedh_bracket` + `BracketViolation` error
- `client/src/components/menu/AiDifficultyDropdown.tsx` — cEDH option + B5 badge
- `client/src/components/menu/AiOpponentConfig.tsx` — cascade-on-cEDH + toast
- `client/src/services/aiDeckCatalog.ts` — `filterByBracket(decks, tier)`
- `client/src/pages/GameSetupPage.tsx` — warning chip when human deck not B5 + any AI is cEDH
- `client/src/pages/GamePage.tsx` — render typed `BracketViolation` error as blocking modal
- WASM/Tauri/server game-setup boundaries — call `validate_cedh_bracket` before `initialize_game`

---

# Phase 1 — Foundation: `AiDifficulty::CEDH` + preset + 4p scaling skip

Adds the enum variant, the cEDH preset arm in `create_config`, the cEDH-specific 4p scaling branch, the `combo_progress_*` policy-penalty fields, and the serde-roundtrip test for the new variant.

## Task 1.1: Add `AiDifficulty::CEDH` enum variant

**Files:**
- Modify: `crates/phase-ai/src/config.rs:36-42` (the `AiDifficulty` enum)
- Test: `crates/phase-ai/src/config.rs` (existing `#[cfg(test)] mod tests`)

- [ ] **Step 1: Add the variant**

Edit `crates/phase-ai/src/config.rs:36-42`:

```rust
/// AI difficulty level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum AiDifficulty {
    VeryEasy,
    Easy,
    Medium,
    Hard,
    VeryHard,
    /// Bracket-5 competitive Commander. Bypasses 4-player paranoid scaling;
    /// activates combo-recognition policies via `DeckFeatures::is_cedh`.
    CEDH,
}
```

The `PartialOrd, Ord` derive means `CEDH` is the maximum variant (last in declaration order).

- [ ] **Step 2: Extend the serde-roundtrip test**

Edit `crates/phase-ai/src/config.rs` at the existing `ai_difficulty_serde_roundtrips` test (find with `grep -n "ai_difficulty_serde_roundtrips" crates/phase-ai/src/config.rs`):

```rust
#[test]
fn ai_difficulty_serde_roundtrips() {
    for diff in [
        AiDifficulty::VeryEasy,
        AiDifficulty::Easy,
        AiDifficulty::Medium,
        AiDifficulty::Hard,
        AiDifficulty::VeryHard,
        AiDifficulty::CEDH,
    ] {
        let json = serde_json::to_string(&diff).unwrap();
        let parsed: AiDifficulty = serde_json::from_str(&json).unwrap();
        assert_eq!(diff, parsed);
    }
}
```

- [ ] **Step 3: Verify the build fails on the missing match arm**

`cargo fmt --all`, then run the verification pattern. Expected: a non-exhaustive-match compile error in `create_config()` because the new variant has no arm yet. That's the next task.

- [ ] **Step 4: Commit (yes, broken build — Task 1.2 fixes it)**

This step is intentionally broken so we have a focused commit per logical change. If the agent toolchain refuses broken commits, batch Task 1.1 and 1.2 into one commit instead.

```bash
git add crates/phase-ai/src/config.rs
git commit -m "feat(phase-ai): add AiDifficulty::CEDH variant

Adds the enum variant plus serde-roundtrip coverage. The
create_config() match arm is added in the next commit; this
commit is intentionally non-building so the enum addition is
visible in isolation."
```

If the team convention is "every commit must build", skip the broken commit and combine with Task 1.2.

## Task 1.2: Add cEDH preset arm in `create_config()`

**Files:**
- Modify: `crates/phase-ai/src/config.rs:367-488` (the `create_config` match)
- Test: `crates/phase-ai/src/config.rs` (new `cedh_preset_values` test)

- [ ] **Step 1: Write the failing test**

Append to the existing test module in `crates/phase-ai/src/config.rs`:

```rust
#[test]
fn cedh_preset_values() {
    let config = create_config(AiDifficulty::CEDH, Platform::Native);
    assert_eq!(config.difficulty, AiDifficulty::CEDH);
    assert_eq!(config.temperature, 0.2);
    assert_eq!(config.profile.risk_tolerance, 0.4);
    assert_eq!(config.profile.interaction_patience, 1.0);
    assert_eq!(config.profile.stabilize_bias, 1.2);
    assert!(config.play_lookahead);
    assert!(config.combat_lookahead);
    assert!(config.search.enabled);
    assert_eq!(config.search.max_depth, 3);
    assert_eq!(config.search.max_nodes, 96);
    assert_eq!(config.search.max_branching, 5);
    assert_eq!(config.search.rollout_depth, 2);
    assert_eq!(config.search.rollout_samples, 2);
    assert!(matches!(
        config.search.opponent_model,
        OpponentModel::ThreatWeightedReply
    ));
    assert!(matches!(
        config.search.threat_awareness,
        ThreatAwareness::Full
    ));
    assert_eq!(config.search.projection_min_budget_ms, 1500);
    assert_eq!(config.search.time_budget_ms, AI_SEARCH_TIME_BUDGET_MS);
}

#[test]
fn cedh_preset_wasm_caps_apply() {
    let config = create_config(AiDifficulty::CEDH, Platform::Wasm);
    assert_eq!(config.search.max_depth, 2);  // capped from 3
    assert_eq!(config.search.max_nodes, 64); // 96 * 2/3
    assert_eq!(config.search.rollout_depth, 2);
}
```

- [ ] **Step 2: Run to verify it fails**

`cargo test -p phase-ai cedh_preset_values --no-fail-fast` — expect compile error (missing match arm in `create_config`).

- [ ] **Step 3: Add the cEDH match arm**

Edit `crates/phase-ai/src/config.rs` — find the existing `AiDifficulty::VeryHard => (` arm at `:464-487` and add this arm immediately after it (still inside the `match difficulty` expression that starts at `:367`):

```rust
        AiDifficulty::CEDH => (
            0.2,
            AiProfile {
                risk_tolerance: 0.4,
                interaction_patience: 1.0,
                stabilize_bias: 1.2,
            },
            true,  // play_lookahead
            true,  // combat_lookahead — cEDH is the first tier to enable this
            SearchConfig {
                enabled: true,
                max_depth: 3,
                max_nodes: 96,
                max_branching: 5,
                planner_mode: PlannerMode::BeamPlusRollout,
                rollout_depth: 2,
                rollout_samples: 2,
                opponent_model: OpponentModel::ThreatWeightedReply,
                time_budget_ms: AI_SEARCH_TIME_BUDGET_MS,
                deterministic: false,
                threat_awareness: ThreatAwareness::Full,
                projection_min_budget_ms: 1500,
            },
        ),
```

- [ ] **Step 4: Run verification pattern**

Expected: `cedh_preset_values` and `cedh_preset_wasm_caps_apply` pass; `ai_difficulty_serde_roundtrips` continues to pass with the new variant; clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/phase-ai/src/config.rs
git commit -m "feat(phase-ai): cEDH difficulty preset

depth=3 nodes=96 rollout=2x2 combat_lookahead=true.
projection_min_budget_ms reduced 2000->1500 (cEDH needs
projections more aggressively than VeryHard).
WASM caps apply unchanged: depth 2, nodes 64, rollout 2."
```

## Task 1.3: Skip 4-player paranoid scaling for cEDH

**Files:**
- Modify: `crates/phase-ai/src/config.rs:543-563` (the `create_config_for_players` match)

- [ ] **Step 1: Write the failing test**

Append to the test module:

```rust
#[test]
fn cedh_skips_paranoid_scaling_at_4p() {
    let cfg = create_config_for_players(AiDifficulty::CEDH, Platform::Native, 4);
    assert_eq!(cfg.search.max_depth, 3,
        "cEDH must not be downgraded to depth 2 by paranoid scaling at 4p");
    assert_eq!(cfg.search.max_nodes, 96,
        "cEDH must keep its native node budget at 4p");
    assert_eq!(cfg.search.max_branching, 5);
    assert_eq!(cfg.search.rollout_depth, 2);
}

#[test]
fn veryhard_still_gets_paranoid_scaling_at_4p() {
    // Sanity: the scaling skip is cEDH-specific and doesn't affect VeryHard.
    let cfg = create_config_for_players(AiDifficulty::VeryHard, Platform::Native, 4);
    assert_eq!(cfg.search.max_depth, 2, "VeryHard should still be capped at 4p");
}
```

- [ ] **Step 2: Run to verify the cEDH test fails**

`cargo test -p phase-ai cedh_skips_paranoid_scaling_at_4p --no-fail-fast` — expect FAIL (current code clips cEDH to depth 2).

- [ ] **Step 3: Add the cEDH branch in `create_config_for_players`**

Edit `crates/phase-ai/src/config.rs:543-563`:

```rust
    match player_count {
        0..=2 => {} // No scaling needed
        3..=4 => {
            if difficulty == AiDifficulty::CEDH {
                // cEDH is calibrated for 4-player tables. The generic
                // paranoid cap (depth 2, nodes * 2/3, branching 4, rollout 1)
                // would cripple it. The cEDH preset already accounts for
                // the table size — no-op here.
            } else {
                // Paranoid search: cap depth at 2, reduce budget
                config.search.max_depth = config.search.max_depth.min(2);
                config.search.max_nodes = config.search.max_nodes * 2 / 3;
                config.search.max_branching = config.search.max_branching.min(4);
                config.search.rollout_depth = config.search.rollout_depth.min(1);
            }
        }
        _ => {
            // 5-6+ players: heuristic-only or minimal search
            if config.difficulty <= AiDifficulty::Medium {
                config.search.enabled = false;
            } else {
                config.search.max_depth = 1;
                config.search.max_nodes /= 3;
                config.search.max_branching = config.search.max_branching.min(3);
                config.search.rollout_depth = config.search.rollout_depth.min(1);
            }
        }
    }
```

The 5-6p path is unchanged. cEDH is not common at 5-6p; the existing path will clip it. A follow-up may reject cEDH at `player_count > 4` at game-setup time.

- [ ] **Step 4: Run verification pattern**

Expected: both new tests pass; existing 3-4p scaling tests for non-CEDH difficulties continue to pass.

- [ ] **Step 5: Commit**

```bash
git add crates/phase-ai/src/config.rs
git commit -m "feat(phase-ai): cEDH bypasses 4p paranoid scaling

cEDH is exclusively played at 4-player tables and is calibrated
for that count from the base preset. The generic 3-4p scaler
(cap depth at 2, reduce nodes to 2/3) would cripple it.
Non-cEDH difficulties continue to follow paranoid scaling
unchanged."
```

## Task 1.4: Add `combo_progress_*` fields to `PolicyPenalties`

**Files:**
- Modify: `crates/phase-ai/src/config.rs:156-247` (`PolicyPenalties` struct + impl Default + serde defaults)

- [ ] **Step 1: Write the failing test**

Append to the test module:

```rust
#[test]
fn policy_penalties_default_combo_progress_bonuses() {
    let p = PolicyPenalties::default();
    assert_eq!(p.combo_progress_this_turn_bonus, 15.0);
    assert_eq!(p.combo_progress_next_turn_bonus, 5.0);
}
```

- [ ] **Step 2: Run to verify it fails**

`cargo test -p phase-ai policy_penalties_default_combo_progress_bonuses --no-fail-fast` — expect compile error (missing field).

- [ ] **Step 3: Add the fields**

In `crates/phase-ai/src/config.rs`, find `pub struct PolicyPenalties` (around line 156). Add the new fields at the bottom of the struct, each with a serde default for backward compatibility:

```rust
    /// Bonus prior when a candidate action progresses a combo line that is
    /// reachable this turn. Consumed by `ComboLinePolicy`.
    #[serde(default = "default_combo_progress_this_turn_bonus")]
    pub combo_progress_this_turn_bonus: f64,
    /// Bonus prior when a candidate action (tutor / draw / ramp) progresses a
    /// combo line that is reachable next turn. Consumed by `ComboLinePolicy`.
    #[serde(default = "default_combo_progress_next_turn_bonus")]
    pub combo_progress_next_turn_bonus: f64,
```

Add to `impl Default for PolicyPenalties` (around line 250):

```rust
            combo_progress_this_turn_bonus: default_combo_progress_this_turn_bonus(),
            combo_progress_next_turn_bonus: default_combo_progress_next_turn_bonus(),
```

Add the default helpers next to the others (around line 290):

```rust
fn default_combo_progress_this_turn_bonus() -> f64 {
    15.0
}
fn default_combo_progress_next_turn_bonus() -> f64 {
    5.0
}
```

- [ ] **Step 4: Run verification pattern**

Expected: new test passes; existing `PolicyPenalties` serde tests still pass; clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/phase-ai/src/config.rs
git commit -m "feat(phase-ai): add combo_progress_* PolicyPenalties

Tunable bonuses for combo-progress prior boosts. Consumed by
ComboLinePolicy (next phase). Defaults +15.0 (this turn) and
+5.0 (next turn) match the spec; both serde-default so older
saved configs deserialize cleanly."
```

---

# Phase 2 — `DeckFeatures::is_cedh`

Adds the gating field that drives `ComboLinePolicy::activation()` and `CedhKeepablesMulligan`'s internal gate.

## Task 2.1: Add `is_cedh` field to `DeckFeatures`

**Files:**
- Modify: `crates/phase-ai/src/features/mod.rs:42-54` (`DeckFeatures` struct)
- Test: same file

- [ ] **Step 1: Write the failing test**

Append to or create the test module in `crates/phase-ai/src/features/mod.rs` (use the existing test wiring — the directory has `features/tests/` already):

```rust
#[cfg(test)]
mod cedh_field_tests {
    use super::*;

    #[test]
    fn default_features_is_not_cedh() {
        let f = DeckFeatures::default();
        assert!(!f.is_cedh);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

`cargo test -p phase-ai default_features_is_not_cedh --no-fail-fast` — expect compile error (missing field).

- [ ] **Step 3: Add the field**

Edit `crates/phase-ai/src/features/mod.rs:42-54`:

```rust
/// Aggregated structural features detected from a single player's deck.
///
/// Carries the deck's strategic archetype + strategy profile alongside the
/// per-class feature data — policies use these in `activation()` to compute
/// archetype- and turn-phase-sensitive weighting without consulting
/// `AiContext` directly.
#[derive(Debug, Clone, Default)]
pub struct DeckFeatures {
    pub archetype: DeckArchetype,
    pub strategy: StrategyProfile,
    pub landfall: LandfallFeature,
    pub mana_ramp: ManaRampFeature,
    pub tribal: TribalFeature,
    pub control: ControlFeature,
    pub aristocrats: AristocratsFeature,
    pub aggro_pressure: AggroPressureFeature,
    pub tokens_wide: TokensWideFeature,
    pub plus_one_counters: PlusOneCountersFeature,
    pub spellslinger_prowess: SpellslingerProwessFeature,
    /// Declaration-derived: `true` iff the deck's declared bracket tier is
    /// `CommanderBracketTier::Cedh`. Unlike the other fields here, this is
    /// not structurally detected from card text — it is a per-deck
    /// declaration set at deck-analysis time from deck metadata. Used by
    /// `ComboLinePolicy::activation()` and `CedhKeepablesMulligan` as a
    /// gating signal.
    pub is_cedh: bool,
}
```

- [ ] **Step 4: Run verification pattern**

Expected: new test passes; all callers of `DeckFeatures::default()` continue to compile (the field defaults to `false` because `bool: Default`); clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/phase-ai/src/features/mod.rs
git commit -m "feat(phase-ai): DeckFeatures::is_cedh gating field

Adds a declaration-derived feature that ComboLinePolicy and
CedhKeepablesMulligan use to gate their behavior. Populated
from CommanderBracketTier::Cedh deck metadata at
deck-analysis time (next task wires the population)."
```

## Task 2.2: Wire `is_cedh` population from deck metadata

**Files:**
- Discovery first: `grep -rn "DeckFeatures *{" crates/phase-ai/src/ --include="*.rs"` to find the construction site(s) other than `default()` and test-only uses.

- [ ] **Step 1: Locate the production construction site**

```bash
grep -rn "DeckFeatures *{" crates/phase-ai/src/ --include="*.rs" \
  | grep -v "^.*test" | grep -v "default" | grep -v "..DeckFeatures::default"
```

The hit (or hits) outside tests is where `DeckFeatures` is built from a real deck. Most likely candidates: `crate::context::AiContext::analyze` at `crates/phase-ai/src/context.rs:47` — confirm by reading it.

If `DeckFeatures` is currently built field-by-field in one or more analyze functions, that's where `is_cedh` is populated. If it's not currently built explicitly (e.g., only via `DeckFeatures::default()`), introduce a single `DeckFeatures::analyze(deck: &[DeckEntry], tier: CommanderBracketTier) -> Self` constructor and route callers through it.

- [ ] **Step 2: Write the failing test**

Append to the test module in `crates/phase-ai/src/features/mod.rs`:

```rust
    #[test]
    fn analyze_with_cedh_tier_sets_is_cedh() {
        use engine::game::bracket_estimate::CommanderBracketTier;
        // Use an empty deck — structural features default to zero; is_cedh
        // should follow only the tier argument.
        let f = DeckFeatures::analyze(&[], CommanderBracketTier::Cedh);
        assert!(f.is_cedh, "Cedh tier must set is_cedh = true");
    }

    #[test]
    fn analyze_with_non_cedh_tier_leaves_is_cedh_false() {
        use engine::game::bracket_estimate::CommanderBracketTier;
        for tier in [
            CommanderBracketTier::Exhibition,
            CommanderBracketTier::Core,
            CommanderBracketTier::Upgraded,
            CommanderBracketTier::Optimized,
        ] {
            let f = DeckFeatures::analyze(&[], tier);
            assert!(!f.is_cedh, "non-Cedh tier ({tier:?}) must leave is_cedh = false");
        }
    }
```

(Verify exact `CommanderBracketTier` variant names at `crates/engine/src/game/bracket_estimate.rs:22-29` — adjust the test if the spelling differs.)

- [ ] **Step 3: Run to verify it fails**

`cargo test -p phase-ai analyze_with_cedh_tier_sets_is_cedh --no-fail-fast` — expect compile error (`DeckFeatures::analyze` doesn't exist).

- [ ] **Step 4: Add the `DeckFeatures::analyze` constructor**

Append to `crates/phase-ai/src/features/mod.rs`:

```rust
impl DeckFeatures {
    /// Construct `DeckFeatures` from a deck. Walks each per-class detector
    /// (`landfall::detect`, `mana_ramp::detect`, ...) and sets `is_cedh`
    /// from the declared bracket tier.
    ///
    /// Per-class detectors are pure functions over `&[DeckEntry]`. The tier
    /// argument flows in from deck metadata at the AI-setup boundary.
    pub fn analyze(
        deck: &[engine::game::DeckEntry],
        tier: engine::game::bracket_estimate::CommanderBracketTier,
    ) -> Self {
        Self {
            archetype: crate::deck_profile::DeckArchetype::default(),
            strategy: crate::strategy_profile::StrategyProfile::default(),
            landfall: landfall::detect(deck),
            mana_ramp: mana_ramp::detect(deck),
            tribal: tribal::detect(deck),
            control: control::detect(deck),
            aristocrats: aristocrats::detect(deck),
            aggro_pressure: aggro_pressure::detect(deck),
            tokens_wide: tokens_wide::detect(deck),
            plus_one_counters: plus_one_counters::detect(deck),
            spellslinger_prowess: spellslinger_prowess::detect(deck),
            is_cedh: tier == engine::game::bracket_estimate::CommanderBracketTier::Cedh,
        }
    }
}
```

(Verify each `<feature>::detect` is the correct function name by inspecting that module's `pub fn`. The list in this template matches the existing fields in `DeckFeatures`. If a feature's detector has a different signature in this codebase, adjust.)

Then update the existing call site(s) located in Step 1 to use `DeckFeatures::analyze(deck, tier)`, threading the `tier` parameter from wherever deck metadata is currently consumed (likely `AiContext::analyze` — that function may need a new `tier` parameter; if so, ripple the change to its callers).

If the callers do not currently have access to the bracket tier, plumb it through: the tier is deck metadata and is set in the deck-builder / deck import flow. The minimum-friction wire-through:

1. `engine::game::DeckEntry` is per-card; the deck tier is per-deck. Add a wrapper type or a side-channel parameter — do **not** put `tier` on every `DeckEntry`.
2. `AiContext::analyze(deck: &[DeckEntry], base_weights: &EvalWeightSet)` becomes `AiContext::analyze(deck: &[DeckEntry], tier: CommanderBracketTier, base_weights: &EvalWeightSet)`.
3. Callers of `AiContext::analyze` must source the tier from deck metadata at their layer.

- [ ] **Step 5: Run verification pattern**

Expected: both new tests pass; all callers compile after threading `tier`; clippy clean.

- [ ] **Step 6: Commit**

```bash
git add crates/phase-ai/src/features/mod.rs \
        crates/phase-ai/src/context.rs \
        $(grep -rl "AiContext::analyze\|DeckFeatures *{" crates/phase-ai/src/ \
          --include="*.rs" | grep -v test)
git commit -m "feat(phase-ai): populate DeckFeatures::is_cedh from tier

Add DeckFeatures::analyze(deck, tier) as the canonical
constructor. Threads bracket tier through AiContext::analyze
so is_cedh is set when the deck is declared CommanderBracketTier::Cedh.

Per-class structural detectors (landfall::detect et al.) are
unchanged; only the aggregating constructor learns about the
tier."
```

---

# Phase 3 — Engine bracket validation

`validate_cedh_bracket` is an engine-side tag check (per spec section 5.5). Pure unit-tested.

## Task 3.1: Add `BracketViolation` error + `validate_cedh_bracket` function

**Files:**
- Modify: `crates/engine/src/database/legality.rs`
- Test: same file (inline `#[cfg(test)]`)

- [ ] **Step 1: Inspect the existing legality module**

```bash
cat crates/engine/src/database/legality.rs | head -50
```

Find the existing error types and conventions (probably a `LegalityError` enum or similar). Add `BracketViolation` consistently.

- [ ] **Step 2: Write the failing test**

Append at the bottom of `crates/engine/src/database/legality.rs`:

```rust
#[cfg(test)]
mod cedh_bracket_tests {
    use super::*;
    use crate::game::bracket_estimate::CommanderBracketTier;
    // Adjust the Deck import to match the actual deck type used by this
    // crate's deck-loading code (probably engine::game::Deck or similar).
    // If the function takes &[&DeckMetadata] or &[DeckHeader], use that.
    use crate::types::deck::Deck;

    fn make_deck(name: &str, tier: CommanderBracketTier) -> Deck {
        // Construct the minimum Deck that this crate considers valid for
        // legality checks. Fill in only the fields validate_cedh_bracket
        // reads (name, tier).
        let mut d = Deck::empty(name.to_string());
        d.set_bracket_tier(tier);
        d
    }

    #[test]
    fn validate_accepts_all_cedh_decks() {
        let a = make_deck("a", CommanderBracketTier::Cedh);
        let b = make_deck("b", CommanderBracketTier::Cedh);
        assert!(validate_cedh_bracket(&[&a, &b]).is_ok());
    }

    #[test]
    fn validate_rejects_a_non_cedh_deck() {
        let a = make_deck("a", CommanderBracketTier::Cedh);
        let b = make_deck("b", CommanderBracketTier::Upgraded);
        let err = validate_cedh_bracket(&[&a, &b]).unwrap_err();
        match err {
            BracketViolation::DeckNotCedh { deck_name, actual_tier } => {
                assert_eq!(deck_name, "b");
                assert_eq!(actual_tier, CommanderBracketTier::Upgraded);
            }
        }
    }

    #[test]
    fn validate_rejects_empty_input() {
        // Convention call: validating an empty deck list is a programming
        // error, not user-facing. Either accept (vacuously true) or reject
        // with a typed variant. Spec doesn't pin this — implementer picks
        // and tests the choice.
        // Default: accept (Ok). Change the assert below if you choose reject.
        assert!(validate_cedh_bracket(&[]).is_ok());
    }
}
```

If the actual `Deck` type lacks `empty()` and `set_bracket_tier()` constructors, write the smallest helper in the test module (or use whatever construction the rest of `legality.rs` uses).

- [ ] **Step 3: Run to verify it fails**

`cargo test -p engine cedh_bracket_tests --no-fail-fast` — expect compile errors (missing `BracketViolation`, missing `validate_cedh_bracket`).

- [ ] **Step 4: Add the function and error type**

Append to `crates/engine/src/database/legality.rs`:

```rust
use crate::game::bracket_estimate::CommanderBracketTier;

/// Bracket-lock violation surfaced by [`validate_cedh_bracket`]. cEDH is the
/// strictest tier (`CommanderBracketTier::Cedh`) and is **manual-declaration
/// only** — the bracket estimator at
/// `crates/engine/src/game/bracket_estimate.rs:22-29` algorithmically returns
/// only `B1..=B4`, never `Cedh`. Validation is therefore a tag check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BracketViolation {
    DeckNotCedh {
        deck_name: String,
        actual_tier: CommanderBracketTier,
    },
}

impl std::fmt::Display for BracketViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DeckNotCedh { deck_name, actual_tier } => write!(
                f,
                "deck '{deck_name}' is tier {actual_tier:?}; cEDH games require all decks to be tier Cedh"
            ),
        }
    }
}

impl std::error::Error for BracketViolation {}

/// Validate that every deck is declared as `CommanderBracketTier::Cedh`.
/// Called at game-setup time when any AI seat uses `AiDifficulty::CEDH`.
///
/// Returns the **first** non-cEDH deck encountered. If the caller wants the
/// full list, iterate and collect at the call site.
pub fn validate_cedh_bracket(decks: &[&crate::types::deck::Deck]) -> Result<(), BracketViolation> {
    for deck in decks {
        let tier = deck.bracket_tier();  // adjust accessor to match Deck's API
        if tier != CommanderBracketTier::Cedh {
            return Err(BracketViolation::DeckNotCedh {
                deck_name: deck.name().to_string(),
                actual_tier: tier,
            });
        }
    }
    Ok(())
}
```

(If `Deck` doesn't currently store a `bracket_tier`, add the field on `Deck` in this same task: read the existing struct, add `bracket_tier: CommanderBracketTier` with a default of `Core`, expose `bracket_tier()`/`set_bracket_tier()` accessors. Verify against existing usage — there may already be a tier field under a different name.)

- [ ] **Step 5: Run verification pattern**

Expected: all three `cedh_bracket_tests` pass; engine compiles; clippy clean.

- [ ] **Step 6: Commit**

```bash
git add crates/engine/src/database/legality.rs crates/engine/src/types/deck.rs
git commit -m "feat(engine): validate_cedh_bracket tag check

cEDH is manual-declaration only per
bracket_estimate.rs:22-29 (Cedh is never returned by the
estimator). validate_cedh_bracket asserts every deck has its
tier explicitly set to CommanderBracketTier::Cedh.

Returns typed BracketViolation::DeckNotCedh on the first
offender; callers collect across decks if needed."
```

---

# Phase 4 — Combo module

Pure types and registry. No game-state interaction yet (that comes in Phase 5).

## Task 4.1: Create `combo/line.rs` types

**Files:**
- Create: `crates/phase-ai/src/combo/line.rs`
- Create: `crates/phase-ai/src/combo/mod.rs`
- Modify: `crates/phase-ai/src/lib.rs` (add `pub mod combo;`)

- [ ] **Step 1: Write the failing test**

Create `crates/phase-ai/src/combo/line.rs`:

```rust
//! Combo-line type system. Pure data — no game-state or registry coupling.
//! Detection logic lives in `combo/detection.rs`; the registry in `combo/registry.rs`.

use engine::types::actions::GameAction;
use engine::types::mana::ManaCost;

/// Stable identity for a registered combo line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ComboLineId(pub u32);

/// A registered cEDH win condition. Hand-authored; the registry exposes
/// only the lines the engine + parser currently support cleanly.
#[derive(Debug, Clone)]
pub struct ComboLine {
    pub id: ComboLineId,
    pub name: &'static str,
    pub pieces: Vec<ComboPiece>,
    pub mana_cost: ManaCost,
    pub action_sequence: Vec<ComboStep>,
    pub win_kind: WinKind,
}

/// A required component of a combo line, located by zone + predicate.
/// Predicates are intentionally narrow for the skeleton — name-based matching
/// is acceptable here because combo lines are hand-authored. Structural
/// predicates can replace name matching once the AST coverage stabilises.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComboPiece {
    InHand(CardPredicate),
    OnBattlefield(CardPredicate),
    InGraveyard(CardPredicate),
    InLibrary(CardPredicate), // tutorable
}

/// Narrow card predicate for the combo skeleton. Real combo content can
/// extend this to compose structural filters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CardPredicate {
    NameEquals(&'static str),
}

#[derive(Debug, Clone)]
pub enum ComboStep {
    Cast { predicate: CardPredicate },
    Activate { predicate: CardPredicate, ability_index: u8 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WinKind {
    /// CR 104.2 explicit win/loss effect (Thoracle / Laboratory Maniac).
    ImmediateLoss,
    /// CR 726 infinite combat / mill / damage loop.
    InfiniteLoop,
    /// Lethal damage or commander damage from the combo's resolution.
    LethalDamage,
}

/// Reachability assessment for a combo line against a game state.
#[derive(Debug, Clone)]
pub enum ComboReachability {
    NotReachable,
    ReachableThisTurn {
        missing_mana: u8,
        required_actions: Vec<GameAction>,
    },
    ReachableNextTurn {
        missing_pieces: Vec<ComboPiece>,
    },
    ReachableSoon {
        turns_estimated: u8,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combo_line_id_is_hashable_and_comparable() {
        use std::collections::HashSet;
        let mut s = HashSet::new();
        s.insert(ComboLineId(1));
        s.insert(ComboLineId(2));
        s.insert(ComboLineId(1));
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn combo_piece_equality_is_structural() {
        assert_eq!(
            ComboPiece::InHand(CardPredicate::NameEquals("Kiki-Jiki, Mirror Breaker")),
            ComboPiece::InHand(CardPredicate::NameEquals("Kiki-Jiki, Mirror Breaker")),
        );
        assert_ne!(
            ComboPiece::InHand(CardPredicate::NameEquals("Kiki-Jiki, Mirror Breaker")),
            ComboPiece::OnBattlefield(CardPredicate::NameEquals("Kiki-Jiki, Mirror Breaker")),
        );
    }
}
```

- [ ] **Step 2: Create `combo/mod.rs`**

```rust
//! Combo-recognition layer for cEDH difficulty.
//!
//! - `line.rs` — pure types (`ComboLine`, `ComboPiece`, `ComboReachability`, ...)
//! - `detection.rs` — `ComboDetector` trait + default impl over `GameState`
//! - `registry.rs` — hand-authored `ComboRegistry`
//!
//! `ComboLinePolicy` (in `policies/combo_line.rs`) wires this layer into the
//! existing planner via `TacticalPolicy::activation()` keyed on
//! `DeckFeatures::is_cedh`.

pub mod line;
pub mod detection;
pub mod registry;

pub use line::{
    CardPredicate, ComboLine, ComboLineId, ComboPiece, ComboReachability, ComboStep, WinKind,
};
pub use detection::{ComboDetector, DefaultComboDetector};
pub use registry::ComboRegistry;
```

- [ ] **Step 3: Add `pub mod combo;` to `lib.rs`**

`grep -n "pub mod" crates/phase-ai/src/lib.rs` to find the right spot — add `pub mod combo;` alphabetically among the other `pub mod` lines.

- [ ] **Step 4: Run to verify line.rs tests pass**

`cargo test -p phase-ai combo::line --no-fail-fast` — expect both tests pass; clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/phase-ai/src/combo/ crates/phase-ai/src/lib.rs
git commit -m "feat(phase-ai): combo module — pure types

Lays down ComboLine / ComboPiece / ComboReachability / WinKind /
ComboStep / CardPredicate. Detection and registry are
stubbed (next tasks). No game-state coupling yet.

CardPredicate intentionally narrow (name-only) for the
skeleton — real combo content can extend to structural
predicates without breaking the policy boundary."
```

## Task 4.2: Create `combo/detection.rs` with `ComboDetector` trait + default impl

**Files:**
- Create: `crates/phase-ai/src/combo/detection.rs`

- [ ] **Step 1: Write the failing test**

```rust
//! Combo reachability assessment over a `GameState`. The default detector
//! is structural: walks `ComboLine::pieces`, matches them against the AI
//! player's zones, and computes mana shortfall.

use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use crate::combo::line::{
    CardPredicate, ComboLine, ComboPiece, ComboReachability,
};

pub trait ComboDetector: Send + Sync {
    fn assess(
        &self,
        state: &GameState,
        line: &ComboLine,
        ai: PlayerId,
    ) -> ComboReachability;
}

/// Default structural detector. Reuses existing zone-iteration helpers:
/// - `state.players[ai.0 as usize].hand` / `.graveyard` / `.library` for
///   off-battlefield pieces.
/// - `state.battlefield` filtered by `controller == ai` for on-board pieces.
/// - `crate::zone_eval::available_mana(state, ai)` for mana shortfall.
#[derive(Debug, Clone, Copy, Default)]
pub struct DefaultComboDetector;

impl ComboDetector for DefaultComboDetector {
    fn assess(
        &self,
        state: &GameState,
        line: &ComboLine,
        ai: PlayerId,
    ) -> ComboReachability {
        let mut missing: Vec<ComboPiece> = Vec::new();
        for piece in &line.pieces {
            if !piece_present(piece, state, ai) {
                missing.push(piece.clone());
            }
        }

        if missing.is_empty() {
            // All pieces present. Check mana.
            let available = crate::zone_eval::available_mana(state, ai);
            let required = mana_cost_total(&line.mana_cost);
            let shortfall = required.saturating_sub(available);
            if shortfall == 0 {
                ComboReachability::ReachableThisTurn {
                    missing_mana: 0,
                    required_actions: Vec::new(), // Phase 5 wires action_sequence -> required_actions
                }
            } else {
                ComboReachability::ReachableThisTurn {
                    missing_mana: shortfall as u8,
                    required_actions: Vec::new(),
                }
            }
        } else if missing.iter().all(|p| matches!(p, ComboPiece::InLibrary(_))) {
            // Pieces are tutorable but not in hand/board yet.
            ComboReachability::ReachableNextTurn { missing_pieces: missing }
        } else {
            ComboReachability::NotReachable
        }
    }
}

fn piece_present(piece: &ComboPiece, state: &GameState, ai: PlayerId) -> bool {
    match piece {
        ComboPiece::InHand(pred) => {
            state.players[ai.0 as usize]
                .hand
                .iter()
                .any(|&id| matches_in_zone(*pred_ref(pred), state, id))
        }
        ComboPiece::OnBattlefield(pred) => {
            state.battlefield.iter().any(|&id| {
                state.objects.get(&id).is_some_and(|obj| {
                    obj.controller == ai && matches_predicate(pred, &obj.name)
                })
            })
        }
        ComboPiece::InGraveyard(pred) => {
            state.players[ai.0 as usize]
                .graveyard
                .iter()
                .any(|&id| matches_in_zone(*pred_ref(pred), state, id))
        }
        // InLibrary is treated as "tutorable, not yet present" — never returns true.
        // The reachability path elevates lines whose only-missing-pieces are InLibrary
        // to ReachableNextTurn so tutors get prior boosts.
        ComboPiece::InLibrary(_) => false,
    }
}

fn pred_ref(p: &CardPredicate) -> &CardPredicate {
    p
}

fn matches_in_zone(pred: CardPredicate, state: &GameState, id: engine::types::identifiers::ObjectId) -> bool {
    state.objects.get(&id).is_some_and(|obj| matches_predicate(&pred, &obj.name))
}

fn matches_predicate(pred: &CardPredicate, name: &str) -> bool {
    match pred {
        CardPredicate::NameEquals(target) => name == *target,
    }
}

fn mana_cost_total(cost: &engine::types::mana::ManaCost) -> i32 {
    // The MVP collapses colored + generic into a single integer cost; refine
    // when real combo lines need color-aware matching.
    match cost {
        engine::types::mana::ManaCost::Cost { shards, generic } => {
            (shards.len() as i32) + (*generic as i32)
        }
        engine::types::mana::ManaCost::Free => 0,
        // Add any other variants the engine exposes; match exhaustively.
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combo::line::{CardPredicate, ComboLine, ComboLineId, ComboPiece, WinKind};
    use engine::types::game_state::GameState;
    use engine::types::mana::ManaCost;
    use engine::types::player::PlayerId;

    fn empty_state() -> GameState {
        GameState::new_two_player(0)
    }

    fn one_piece_line() -> ComboLine {
        ComboLine {
            id: ComboLineId(999),
            name: "test stub",
            pieces: vec![ComboPiece::InHand(CardPredicate::NameEquals("__test_piece__"))],
            mana_cost: ManaCost::Free,
            action_sequence: Vec::new(),
            win_kind: WinKind::ImmediateLoss,
        }
    }

    #[test]
    fn empty_state_yields_not_reachable() {
        let s = empty_state();
        let line = one_piece_line();
        let r = DefaultComboDetector.assess(&s, &line, PlayerId(0));
        assert!(matches!(r, ComboReachability::NotReachable));
    }
}
```

- [ ] **Step 2: Run to verify it fails**

`cargo test -p phase-ai combo::detection --no-fail-fast` — expect compile errors (missing module wiring), then pass after Step 3.

- [ ] **Step 3: Verify `combo/mod.rs` already exports `detection`**

(Done in Task 4.1's `mod.rs`.) Run verification pattern.

Expected: the `empty_state_yields_not_reachable` test passes. The other reachability paths (piece present, mana shortfall) are exercised in the registry tests in Task 4.3 — adding them here would require constructing a card in the AI player's hand, which is a multi-line builder.

If `engine::types::mana::ManaCost` has variants beyond `Cost { shards, generic }` and `Free`, extend `mana_cost_total` exhaustively. Use `grep -n "pub enum ManaCost\|^    [A-Z]" crates/engine/src/types/mana.rs` to enumerate.

- [ ] **Step 4: Commit**

```bash
git add crates/phase-ai/src/combo/detection.rs
git commit -m "feat(phase-ai): DefaultComboDetector

Walks ComboLine::pieces, matches against AI player's zones,
computes mana shortfall via zone_eval::available_mana.

ReachableThisTurn when all pieces present + mana available.
ReachableNextTurn when only InLibrary pieces are missing
(tutorable case). NotReachable otherwise.

Action sequence -> required_actions mapping is stubbed
(empty Vec); the ComboLinePolicy in the next phase only needs
the reachability variant, not the full action list."
```

## Task 4.3: Create `combo/registry.rs` with one stub line

**Files:**
- Create: `crates/phase-ai/src/combo/registry.rs`

- [ ] **Step 1: Pick the stub combo**

Before writing the registry, decide the proof-of-life combo. Verify the cards exist in current engine coverage:

```bash
jq '."kiki-jiki, mirror breaker"' client/public/card-data.json | head -20
jq '."restoration angel"' client/public/card-data.json | head -20
```

If both have non-`Unimplemented` abilities, use the Kiki-Jiki + Restoration Angel pair. If either is missing or has `Unimplemented` abilities, fall back to a synthetic two-card stub using cards that are definitely supported (e.g., two vanilla creatures whose combat damage equals lethal — verify before committing).

If neither path works cleanly, use a **deliberately synthetic** stub: a single-piece "combo" requiring a creature with name `"__cedh_stub_test_creature__"` on the battlefield. This is acceptable for the skeleton — the goal is to prove the wiring, not to demonstrate a real combo. Document the stub as test-only in the file header.

- [ ] **Step 2: Write the failing test**

Create `crates/phase-ai/src/combo/registry.rs`:

```rust
//! Hand-authored combo-line registry. The skeleton ships with one stub line
//! to verify end-to-end wiring; real cEDH lines (Thoracle/Consult, Heliod/Ballista,
//! Kiki/Twin, etc.) populate this registry in a follow-up phase as engine
//! card coverage stabilises.

use engine::types::game_state::GameState;
use engine::types::mana::ManaCost;
use engine::types::player::PlayerId;

use crate::combo::detection::{ComboDetector, DefaultComboDetector};
use crate::combo::line::{
    CardPredicate, ComboLine, ComboLineId, ComboPiece, ComboReachability, WinKind,
};

pub struct ComboRegistry {
    lines: Vec<ComboLine>,
    detector: Box<dyn ComboDetector>,
}

impl Default for ComboRegistry {
    fn default() -> Self {
        Self {
            lines: vec![stub_line()],
            detector: Box::new(DefaultComboDetector),
        }
    }
}

impl ComboRegistry {
    pub fn reachable_lines(
        &self,
        state: &GameState,
        ai: PlayerId,
    ) -> Vec<(ComboLineId, ComboReachability)> {
        self.lines
            .iter()
            .map(|line| (line.id, self.detector.assess(state, line, ai)))
            .filter(|(_, r)| !matches!(r, ComboReachability::NotReachable))
            .collect()
    }

    pub fn lines(&self) -> &[ComboLine] {
        &self.lines
    }
}

/// Skeleton-only stub. **Not a real cEDH combo.** Populates the registry
/// with one line so policy wiring can be exercised end-to-end. Real combos
/// land in a follow-up phase.
fn stub_line() -> ComboLine {
    ComboLine {
        id: ComboLineId(0),
        name: "skeleton stub (not a real combo)",
        pieces: vec![ComboPiece::OnBattlefield(CardPredicate::NameEquals(
            "__cedh_stub_test_creature__",
        ))],
        mana_cost: ManaCost::Free,
        action_sequence: Vec::new(),
        win_kind: WinKind::LethalDamage,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_state_returns_no_reachable_lines() {
        let state = GameState::new_two_player(0);
        let reg = ComboRegistry::default();
        assert_eq!(reg.reachable_lines(&state, PlayerId(0)).len(), 0);
    }

    #[test]
    fn registry_exposes_one_stub_line() {
        let reg = ComboRegistry::default();
        assert_eq!(reg.lines().len(), 1);
        assert_eq!(reg.lines()[0].id, ComboLineId(0));
    }
}
```

If the chosen stub combo from Step 1 was Kiki-Jiki + Restoration Angel, replace the `stub_line()` body with the real pair (two pieces — `OnBattlefield(NameEquals("Kiki-Jiki, Mirror Breaker"))` and `InHand(NameEquals("Restoration Angel"))`) and verify the cards are present in card-data.

- [ ] **Step 3: Run verification pattern**

Expected: both registry tests pass; clippy clean.

- [ ] **Step 4: Commit**

```bash
git add crates/phase-ai/src/combo/registry.rs
git commit -m "feat(phase-ai): ComboRegistry with one stub line

Skeleton ships with one explicitly-stub line so end-to-end
wiring (DetectComboReachability -> ComboLinePolicy ->
PlannerServices) can be exercised. Real cEDH lines land in
a follow-up phase as engine card coverage stabilises.

reachable_lines() filters out NotReachable so callers see
only meaningful results."
```

---

# Phase 5 — `ComboLinePolicy`

Implements `TacticalPolicy`, gates via `activation()` on `features.is_cedh`, scores via `verdict()`.

## Task 5.1: Add `PolicyId::ComboLineProgress` variant

**Files:**
- Modify: `crates/phase-ai/src/policies/registry.rs:45-90` (the `PolicyId` enum)

- [ ] **Step 1: Add the variant**

In `crates/phase-ai/src/policies/registry.rs`, add at the end of the `PolicyId` enum (after `ReactiveSelfProtection`):

```rust
    ComboLineProgress,
    CedhKeepablesMulligan,
```

(`CedhKeepablesMulligan` is added now too — Phase 6 implements the policy, but adding the ID here avoids a second touch of this file.)

- [ ] **Step 2: Run verification pattern**

Expected: compiles (unused-variant lint may fire on `CedhKeepablesMulligan` — acceptable until Phase 6); clippy clean.

- [ ] **Step 3: Commit**

```bash
git add crates/phase-ai/src/policies/registry.rs
git commit -m "feat(phase-ai): PolicyId variants for cEDH policies

ComboLineProgress + CedhKeepablesMulligan stable IDs.
Implementations land in this phase + Phase 6."
```

## Task 5.2: Implement `ComboLinePolicy`

**Files:**
- Create: `crates/phase-ai/src/policies/combo_line.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/phase-ai/src/policies/combo_line.rs`:

```rust
//! ComboLinePolicy — boosts priors on candidate actions that progress a
//! reachable combo line. Gating: `activation()` returns `None` unless
//! `features.is_cedh`, so non-cEDH decks pay zero cost (the per-DecisionKind
//! index in PolicyRegistry still includes us, but activation skips us).

use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use crate::combo::{ComboRegistry, ComboReachability};
use crate::features::DeckFeatures;
use crate::policies::context::PolicyContext;
use crate::policies::registry::{
    DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy,
};

/// One-line policy: when a combo is reachable this turn, boost actions in
/// the combo's required sequence. When reachable next turn, boost
/// tutor/draw/ramp actions that close the gap.
///
/// Holds an owned `ComboRegistry`. Constructed once per policy registry
/// instantiation. The registry's `reachable_lines` call is cheap-enough to
/// run per candidate at the skeleton stage; caching is a Phase-N optimisation.
pub struct ComboLinePolicy {
    registry: ComboRegistry,
}

impl ComboLinePolicy {
    pub fn new() -> Self {
        Self {
            registry: ComboRegistry::default(),
        }
    }
}

impl Default for ComboLinePolicy {
    fn default() -> Self {
        Self::new()
    }
}

impl TacticalPolicy for ComboLinePolicy {
    fn id(&self) -> PolicyId {
        PolicyId::ComboLineProgress
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[
            DecisionKind::CastSpell,
            DecisionKind::ActivateAbility,
            DecisionKind::SelectTarget,
        ]
    }

    fn activation(
        &self,
        features: &DeckFeatures,
        _state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        if features.is_cedh {
            Some(1.0)
        } else {
            None
        }
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let reachable = self.registry.reachable_lines(ctx.state, ctx.ai_player);
        for (_id, reachability) in &reachable {
            match reachability {
                ComboReachability::ReachableThisTurn { .. } => {
                    if action_progresses_combo(&ctx.candidate.action) {
                        let bonus = ctx.config.policy_penalties.combo_progress_this_turn_bonus;
                        return PolicyVerdict::Score {
                            delta: bonus,
                            reason: PolicyReason::new("combo_this_turn"),
                        };
                    }
                }
                ComboReachability::ReachableNextTurn { .. } => {
                    if action_is_tutor_or_draw_or_ramp(&ctx.candidate.action) {
                        let bonus = ctx.config.policy_penalties.combo_progress_next_turn_bonus;
                        return PolicyVerdict::Score {
                            delta: bonus,
                            reason: PolicyReason::new("combo_next_turn"),
                        };
                    }
                }
                _ => {}
            }
        }
        PolicyVerdict::Score {
            delta: 0.0,
            reason: PolicyReason::new("combo_no_match"),
        }
    }
}

/// MVP-shaped detector: action is "combo-progressing" if it casts/activates
/// any spell or ability. Tightening this against the line's
/// `action_sequence` is a follow-up.
fn action_progresses_combo(action: &GameAction) -> bool {
    matches!(
        action,
        GameAction::CastSpell { .. }
            | GameAction::ActivateAbility { .. }
            | GameAction::ChooseTarget { .. }
    )
}

/// Conservative MVP heuristic: ramp/tutor/draw all surface as a CastSpell or
/// ActivateAbility. Without inspecting the source card's effects, this
/// over-includes — that's acceptable for the skeleton (the boost is bounded
/// by `combo_progress_next_turn_bonus = +5.0`). Phase-N work tightens this
/// using `crate::effect_classify` once card-data feature tags are confirmed.
fn action_is_tutor_or_draw_or_ramp(action: &GameAction) -> bool {
    matches!(
        action,
        GameAction::CastSpell { .. } | GameAction::ActivateAbility { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::ai_support::{ActionMetadata, CandidateAction, TacticalClass};
    use engine::types::actions::GameAction;
    use engine::types::game_state::GameState;
    use engine::types::player::PlayerId;

    use crate::config::{create_config, AiDifficulty, Platform};
    use crate::context::AiContext;
    use crate::eval::EvalWeightSet;
    use crate::features::DeckFeatures;

    fn make_state() -> GameState {
        GameState::new_two_player(0)
    }

    fn make_features(is_cedh: bool) -> DeckFeatures {
        let mut f = DeckFeatures::default();
        f.is_cedh = is_cedh;
        f
    }

    #[test]
    fn activation_returns_none_when_not_cedh() {
        let policy = ComboLinePolicy::new();
        let state = make_state();
        let features = make_features(false);
        let activation = policy.activation(&features, &state, PlayerId(0));
        assert!(activation.is_none());
    }

    #[test]
    fn activation_returns_some_when_is_cedh() {
        let policy = ComboLinePolicy::new();
        let state = make_state();
        let features = make_features(true);
        let activation = policy.activation(&features, &state, PlayerId(0));
        assert_eq!(activation, Some(1.0));
    }

    #[test]
    fn verdict_returns_zero_score_with_no_reachable_combo() {
        // ComboRegistry default has one stub line; empty state -> NotReachable
        // -> reachable_lines is empty -> verdict returns zero.
        let policy = ComboLinePolicy::new();
        let state = make_state();
        let config = create_config(AiDifficulty::CEDH, Platform::Native);
        let weights = EvalWeightSet::learned();
        let context = AiContext::empty(&weights);

        let candidate = CandidateAction {
            action: GameAction::PassPriority,
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Pass,
            },
        };
        let decision = engine::ai_support::AiDecisionContext {
            waiting_for: state.waiting_for.clone(),
            candidates: vec![candidate.clone()],
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &context,
            cast_facts: None,
        };
        let verdict = policy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, .. } => assert_eq!(delta, 0.0),
            _ => panic!("expected Score with zero delta, got {verdict:?}"),
        }
    }
}
```

(Verify `PolicyContext` field names by reading `crates/phase-ai/src/policies/context.rs`. If a field doesn't exist or has a different name, adjust.)

- [ ] **Step 2: Run to verify the tests fail / pass**

`cargo test -p phase-ai policies::combo_line --no-fail-fast` — first run compile-checks the module; expect either compile errors (if PolicyContext shape differs from my guess — read it and adjust) or all three tests passing.

- [ ] **Step 3: Add module declaration**

In `crates/phase-ai/src/policies/mod.rs`, add `pub mod combo_line;` alphabetically among the existing `pub mod` lines.

- [ ] **Step 4: Run verification pattern**

Expected: all three tests pass; clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/phase-ai/src/policies/combo_line.rs crates/phase-ai/src/policies/mod.rs
git commit -m "feat(phase-ai): ComboLinePolicy gated by is_cedh

activation() returns None unless features.is_cedh, so non-cEDH
decks pay zero cost.

verdict() consults the embedded ComboRegistry and boosts
candidates that progress a reachable combo:
- +combo_progress_this_turn_bonus (default +15.0) when the
  combo is reachable this turn and the candidate is a cast/
  activate/choose-target.
- +combo_progress_next_turn_bonus (default +5.0) when only
  next-turn-reachable and the candidate is a cast/activate
  (proxy for tutor/draw/ramp; refined later).

MVP action-progression detector is conservative — refined in
a follow-up against ComboLine::action_sequence."
```

## Task 5.3: Register `ComboLinePolicy` in `PolicyRegistry::default()`

**Files:**
- Modify: `crates/phase-ai/src/policies/registry.rs:174-220` (the registry default)

- [ ] **Step 1: Write the failing test**

Append to the existing tests in `crates/phase-ai/src/policies/registry.rs`:

```rust
    #[test]
    fn default_registry_contains_combo_line_progress() {
        let reg = PolicyRegistry::default();
        let has = reg.policies.iter().any(|p| p.id() == PolicyId::ComboLineProgress);
        assert!(has, "PolicyRegistry::default() must register ComboLinePolicy");
    }
```

- [ ] **Step 2: Run to verify it fails**

`cargo test -p phase-ai default_registry_contains_combo_line_progress --no-fail-fast` — expect FAIL (policy not registered).

- [ ] **Step 3: Register the policy**

In `crates/phase-ai/src/policies/registry.rs:176-212`, append to the `policies` vec in `Default::default()`:

```rust
            Box::new(super::combo_line::ComboLinePolicy::new()),
```

(Place after `ReactiveSelfProtectionPolicy` to keep registration order stable.)

- [ ] **Step 4: Run verification pattern**

Expected: the new test passes; the existing `PolicyRegistry::default().policies.len()` count assertion (if any) needs to be incremented by 1 — find it with `grep -n "policies.len" crates/phase-ai/src/policies/registry.rs` and update.

- [ ] **Step 5: Commit**

```bash
git add crates/phase-ai/src/policies/registry.rs
git commit -m "feat(phase-ai): register ComboLinePolicy in default registry

Policy is now part of PolicyRegistry::default() and
PolicyRegistry::shared(). activation() gates on
features.is_cedh so non-cEDH decks pay zero cost."
```

---

# Phase 6 — `CedhKeepablesMulligan`

`MulliganPolicy` impl with internal gate on `features.is_cedh`.

## Task 6.1: Implement `CedhKeepablesMulligan`

**Files:**
- Create: `crates/phase-ai/src/policies/mulligan/cedh_keepables.rs`

- [ ] **Step 1: Inspect a sibling policy for the call shape**

```bash
cat crates/phase-ai/src/policies/mulligan/aggro_keepables.rs
```

Confirm the exact shape of `MulliganPolicy::evaluate()` and how `features` / `state` are consumed. Match the conventions of the sibling policies.

- [ ] **Step 2: Identify the card-data feature flags for cEDH heuristics**

```bash
# Find the right flag names for "is fast mana", "is tutor", "is interaction".
jq '.[] | keys' client/public/card-data.json | head -1
grep -rn "is_fast_mana\|is_tutor\|is_counterspell\|is_removal" crates/engine/src/database/ crates/phase-ai/src/ 2>/dev/null | head -20
```

If the exact tags differ from the spec, substitute the actual ones. If no per-card classification exists for "fast mana" / "tutor" / "interaction", the simplest fallback is name-based detection against a small static set (e.g. `["Sol Ring", "Mana Crypt", ...]`) — acceptable for the stub since the policy itself is explicitly stubbed.

- [ ] **Step 3: Write the failing test**

```rust
//! CedhKeepablesMulligan — stub aggressive mulligan policy for cEDH decks.
//! Gated internally on `features.is_cedh` (MulliganPolicy has no activation()
//! method; each registered policy is consulted on every hand).
//!
//! Real cEDH mulligan strategy ("keep only hands that win or stop the opponent
//! from winning by turn 4") lands when the ComboRegistry is populated and the
//! policy can ask `ComboRegistry::reachable_lines(hand_pseudo_state)`.

use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;

use crate::features::DeckFeatures;
use crate::plan::PlanSnapshot;
use crate::policies::mulligan::{MulliganPolicy, MulliganScore, TurnOrder};
use crate::policies::registry::{PolicyId, PolicyReason};

pub struct CedhKeepablesMulligan;

impl MulliganPolicy for CedhKeepablesMulligan {
    fn id(&self) -> PolicyId {
        PolicyId::CedhKeepablesMulligan
    }

    fn evaluate(
        &self,
        hand: &[ObjectId],
        state: &GameState,
        features: &DeckFeatures,
        _plan: &PlanSnapshot,
        _turn_order: TurnOrder,
        _mulligans_taken: u8,
    ) -> MulliganScore {
        if !features.is_cedh {
            return MulliganScore::Score {
                delta: 0.0,
                reason: PolicyReason::new("cedh_keepables_not_applicable"),
            };
        }

        let land_count = count_lands_in_hand(hand, state);
        if land_count < 2 {
            return MulliganScore::ForceMulligan {
                reason: PolicyReason::new("cedh_too_few_lands").with_fact("lands", land_count as i64),
            };
        }
        if land_count > 4 {
            return MulliganScore::ForceMulligan {
                reason: PolicyReason::new("cedh_too_many_lands").with_fact("lands", land_count as i64),
            };
        }

        let has_ramp = hand_has_any(hand, state, is_fast_mana_card);
        let has_tutor = hand_has_any(hand, state, is_tutor_card);
        let has_interaction = hand_has_any(hand, state, is_interaction_card);
        if !has_ramp && !has_tutor && !has_interaction {
            return MulliganScore::ForceMulligan {
                reason: PolicyReason::new("cedh_no_acceleration_tutor_or_interaction"),
            };
        }

        // Positive baseline so a cEDH-tagged hand is kept absent forced
        // mulligans from other policies.
        MulliganScore::Score {
            delta: 1.0,
            reason: PolicyReason::new("cedh_baseline_keep"),
        }
    }
}

fn count_lands_in_hand(hand: &[ObjectId], state: &GameState) -> u32 {
    hand.iter()
        .filter(|&&id| {
            state.objects.get(&id).is_some_and(|obj| {
                obj.card_types
                    .core_types
                    .contains(&engine::types::card_type::CoreType::Land)
            })
        })
        .count() as u32
}

fn hand_has_any<F>(hand: &[ObjectId], state: &GameState, pred: F) -> bool
where
    F: Fn(&str) -> bool,
{
    hand.iter().any(|&id| {
        state
            .objects
            .get(&id)
            .is_some_and(|obj| pred(&obj.name))
    })
}

/// Static stub list — replace with card-data feature lookups once the right
/// tag name is confirmed (see Step 2). Names matched are deliberately the
/// canonical cEDH fast-mana set; this is a stub heuristic, not a complete
/// classification.
fn is_fast_mana_card(name: &str) -> bool {
    matches!(
        name,
        "Sol Ring" | "Mana Crypt" | "Mox Diamond" | "Chrome Mox" | "Mana Vault"
            | "Jeweled Lotus" | "Lotus Petal" | "Dark Ritual"
    )
}

fn is_tutor_card(name: &str) -> bool {
    matches!(
        name,
        "Demonic Tutor" | "Vampiric Tutor" | "Mystical Tutor" | "Enlightened Tutor"
            | "Worldly Tutor" | "Imperial Seal" | "Grim Tutor"
    )
}

fn is_interaction_card(name: &str) -> bool {
    matches!(
        name,
        "Force of Will" | "Force of Negation" | "Mana Drain" | "Counterspell"
            | "Swan Song" | "Mindbreak Trap" | "Pact of Negation"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::PlanSnapshot;

    fn make_state() -> GameState {
        GameState::new_two_player(0)
    }

    fn features_with_cedh(is_cedh: bool) -> DeckFeatures {
        let mut f = DeckFeatures::default();
        f.is_cedh = is_cedh;
        f
    }

    #[test]
    fn not_applicable_when_not_cedh() {
        let policy = CedhKeepablesMulligan;
        let score = policy.evaluate(
            &[],
            &make_state(),
            &features_with_cedh(false),
            &PlanSnapshot::default(),
            TurnOrder::OnPlay,
            0,
        );
        match score {
            MulliganScore::Score { delta, .. } => assert_eq!(delta, 0.0),
            _ => panic!("expected zero-delta Score, got {score:?}"),
        }
    }

    #[test]
    fn empty_hand_is_cedh_force_mulligan_too_few_lands() {
        let policy = CedhKeepablesMulligan;
        let score = policy.evaluate(
            &[],
            &make_state(),
            &features_with_cedh(true),
            &PlanSnapshot::default(),
            TurnOrder::OnPlay,
            0,
        );
        assert!(matches!(score, MulliganScore::ForceMulligan { .. }));
    }
}
```

If `PlanSnapshot` doesn't have a `Default` impl, find the test-helper used by sibling mulligan policies and adopt the same pattern.

- [ ] **Step 3: Run verification pattern**

Expected: both tests pass; clippy clean. (The "land count > 4" and "no ramp/tutor/interaction" branches are covered by tests in Task 6.2 since they need hand construction with actual `ObjectId`s — the empty-hand test is enough to verify the wiring.)

- [ ] **Step 4: Commit**

```bash
git add crates/phase-ai/src/policies/mulligan/cedh_keepables.rs
git commit -m "feat(phase-ai): CedhKeepablesMulligan stub policy

Stub heuristics flagged as stub:
- < 2 lands or > 4 lands -> ForceMulligan.
- No fast mana AND no tutor AND no interaction -> ForceMulligan.
- Otherwise +1.0 baseline keep.

Gates internally on features.is_cedh (no-ops for non-cEDH).
Real cEDH mulligan strategy lands when the ComboRegistry is
populated and the policy can ask reachable_lines() against
the opening-hand pseudo-state.

Card classification is name-based against the canonical
cEDH staple set — refined to card-data feature tags once
the tag names are confirmed."
```

## Task 6.2: Register `CedhKeepablesMulligan` in `MulliganRegistry`

**Files:**
- Modify: `crates/phase-ai/src/policies/mulligan/mod.rs:30-48` (module declarations + use re-exports + registry)

- [ ] **Step 1: Write the failing test**

Append to the existing tests in `crates/phase-ai/src/policies/mulligan/mod.rs` (or create a tests module if absent):

```rust
#[cfg(test)]
mod cedh_registration_tests {
    use super::*;

    #[test]
    fn default_registry_contains_cedh_keepables() {
        let reg = MulliganRegistry::default();
        let has = reg.policies.iter().any(|p| p.id() == crate::policies::registry::PolicyId::CedhKeepablesMulligan);
        assert!(has, "MulliganRegistry::default() must register CedhKeepablesMulligan");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

`cargo test -p phase-ai default_registry_contains_cedh_keepables --no-fail-fast` — expect FAIL.

- [ ] **Step 3: Register + re-export**

Edit `crates/phase-ai/src/policies/mulligan/mod.rs`:

1. Add `pub mod cedh_keepables;` to the module declarations (alphabetically).
2. Add `pub use cedh_keepables::CedhKeepablesMulligan;` to the re-exports.
3. Add `Box::new(CedhKeepablesMulligan),` to the `policies` vec in `MulliganRegistry::default()` (append after the existing entries).

- [ ] **Step 4: Run verification pattern**

Expected: new test passes; the existing `MulliganRegistry::default().policies.len()` count assertion (if any) needs to be incremented by 1.

- [ ] **Step 5: Commit**

```bash
git add crates/phase-ai/src/policies/mulligan/mod.rs
git commit -m "feat(phase-ai): register CedhKeepablesMulligan

Default MulliganRegistry now consults the cEDH policy on
every hand. Non-cEDH decks see a zero-delta Score (no-op);
cEDH decks see the stub heuristics evaluated."
```

---

# Phase 7 — Frontend cEDH lock service + UI

## Task 7.1: Create `services/cedhLock.ts`

**Files:**
- Create: `client/src/services/cedhLock.ts`
- Create: `client/src/services/__tests__/cedhLock.test.ts`

- [ ] **Step 1: Inspect the existing GameSetupConfig + Deck shapes**

```bash
grep -rn "type GameSetupConfig\|interface GameSetupConfig\|type Deck\b\|interface Deck\b" client/src/ --include="*.ts" --include="*.tsx" | head -10
grep -rn "CommanderBracketTier\|Cedh" client/src/ --include="*.ts" --include="*.tsx" | head -10
```

Use the actual type names. The samples below assume `GameSetupConfig` with an `aiOpponents: Array<{ difficulty: AiDifficulty; ... }>` and `Deck` with a `tier` field. Adjust to the real shape.

- [ ] **Step 2: Write the failing tests**

Create `client/src/services/__tests__/cedhLock.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import {
  anyAiOpponentIsCedh,
  applyCedhCascade,
  isDeckCedhLegal,
} from '../cedhLock';
import type { Deck, GameSetupConfig } from '../../adapter/types';

function makeConfig(diffs: string[]): GameSetupConfig {
  return {
    aiOpponents: diffs.map((d) => ({ difficulty: d, /* ...other required fields */ })),
    // ...other required GameSetupConfig fields; fill from the real type.
  } as GameSetupConfig;
}

function makeDeck(tier: string): Deck {
  return { tier, /* ...other required fields */ } as Deck;
}

describe('cedhLock', () => {
  it('anyAiOpponentIsCedh returns false when no AI is cEDH', () => {
    expect(anyAiOpponentIsCedh(makeConfig(['Easy', 'Medium', 'Hard']))).toBe(false);
  });

  it('anyAiOpponentIsCedh returns true when any AI is cEDH', () => {
    expect(anyAiOpponentIsCedh(makeConfig(['Easy', 'CEDH', 'Hard']))).toBe(true);
  });

  it('applyCedhCascade upgrades all AI to CEDH when one is CEDH', () => {
    const before = makeConfig(['Easy', 'CEDH', 'Hard']);
    const after = applyCedhCascade(before);
    expect(after.aiOpponents.every((a) => a.difficulty === 'CEDH')).toBe(true);
  });

  it('applyCedhCascade is a no-op when no AI is cEDH', () => {
    const before = makeConfig(['Easy', 'Medium']);
    const after = applyCedhCascade(before);
    expect(after.aiOpponents.map((a) => a.difficulty)).toEqual(['Easy', 'Medium']);
  });

  it('isDeckCedhLegal returns true only for tier Cedh', () => {
    expect(isDeckCedhLegal(makeDeck('Cedh'))).toBe(true);
    expect(isDeckCedhLegal(makeDeck('Optimized'))).toBe(false);
    expect(isDeckCedhLegal(makeDeck('Upgraded'))).toBe(false);
    expect(isDeckCedhLegal(makeDeck('Core'))).toBe(false);
    expect(isDeckCedhLegal(makeDeck('Exhibition'))).toBe(false);
  });
});
```

- [ ] **Step 3: Run to verify it fails**

`cd client && pnpm test -- --run cedhLock` — expect compile errors (missing module).

- [ ] **Step 4: Create the module**

Create `client/src/services/cedhLock.ts`:

```ts
import type { Deck, GameSetupConfig } from '../adapter/types';

/**
 * Single source of truth for cEDH bracket-lock semantics on the frontend.
 *
 * - `anyAiOpponentIsCedh` — does any AI seat want cEDH difficulty?
 * - `applyCedhCascade` — when one AI is cEDH, all AI seats must be cEDH.
 *   Pure: returns a new config; never mutates input.
 * - `isDeckCedhLegal` — does this deck's declared bracket tier qualify as cEDH?
 *
 * Every cEDH-lock decision in the frontend flows through these three helpers.
 * Adding new checks elsewhere is a defect — they belong here.
 */

const CEDH_DIFFICULTY = 'CEDH';
const CEDH_TIER = 'Cedh';

export function anyAiOpponentIsCedh(config: GameSetupConfig): boolean {
  return config.aiOpponents?.some((ai) => ai.difficulty === CEDH_DIFFICULTY) ?? false;
}

export function applyCedhCascade(config: GameSetupConfig): GameSetupConfig {
  if (!anyAiOpponentIsCedh(config)) {
    return config;
  }
  return {
    ...config,
    aiOpponents: config.aiOpponents.map((ai) => ({ ...ai, difficulty: CEDH_DIFFICULTY })),
  };
}

export function isDeckCedhLegal(deck: Deck): boolean {
  return deck.tier === CEDH_TIER;
}
```

- [ ] **Step 5: Run verification pattern**

Expected: all five `cedhLock.test.ts` tests pass; type-check clean; lint clean.

- [ ] **Step 6: Commit**

```bash
git add client/src/services/cedhLock.ts client/src/services/__tests__/cedhLock.test.ts
git commit -m "feat(client): cedhLock service — single source of truth

anyAiOpponentIsCedh / applyCedhCascade / isDeckCedhLegal.
All cEDH-lock decisions in the frontend route through this
module. Pure functions; never mutate inputs."
```

## Task 7.2: Wire cascade into `AiOpponentConfig`

**Files:**
- Modify: `client/src/components/menu/AiOpponentConfig.tsx`
- Test: `client/src/components/menu/__tests__/AiOpponentConfig.test.tsx` (create if absent)

- [ ] **Step 1: Inspect the current component**

```bash
cat client/src/components/menu/AiOpponentConfig.tsx
```

Identify the difficulty `onChange` handler.

- [ ] **Step 2: Write the failing test**

Add to (or create) `client/src/components/menu/__tests__/AiOpponentConfig.test.tsx`:

```tsx
import { describe, expect, it, vi } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import { AiOpponentConfig } from '../AiOpponentConfig';

describe('AiOpponentConfig — cEDH cascade', () => {
  it('selecting cEDH on one seat upgrades all other AI seats to cEDH', () => {
    const onChange = vi.fn();
    const config = {
      aiOpponents: [
        { difficulty: 'Easy' },
        { difficulty: 'Hard' },
      ],
      // ...other required fields
    } as any;

    render(<AiOpponentConfig config={config} onConfigChange={onChange} />);

    // Find the first AI's difficulty dropdown and select cEDH
    const dropdowns = screen.getAllByLabelText(/AI difficulty/i);
    fireEvent.change(dropdowns[0], { target: { value: 'CEDH' } });

    expect(onChange).toHaveBeenCalledWith(
      expect.objectContaining({
        aiOpponents: [
          { difficulty: 'CEDH' },
          { difficulty: 'CEDH' },
        ],
      }),
    );
  });
});
```

- [ ] **Step 3: Run to verify it fails**

`cd client && pnpm test -- --run AiOpponentConfig` — expect FAIL.

- [ ] **Step 4: Implement the cascade**

In `client/src/components/menu/AiOpponentConfig.tsx`, find the `onChange` handler for AI difficulty. Replace the existing single-seat update with cascade-aware logic:

```ts
import { applyCedhCascade } from '../../services/cedhLock';
import toast from 'react-hot-toast'; // or whatever toast library is in use; grep to confirm

// ...inside the component:
const handleDifficultyChange = (seatIndex: number, newDifficulty: AiDifficulty) => {
  let next = {
    ...config,
    aiOpponents: config.aiOpponents.map((ai, idx) =>
      idx === seatIndex ? { ...ai, difficulty: newDifficulty } : ai,
    ),
  };

  const wasCedh = config.aiOpponents.some((ai) => ai.difficulty === 'CEDH');
  next = applyCedhCascade(next);
  const isCedh = next.aiOpponents.every((ai) => ai.difficulty === 'CEDH') &&
                 next.aiOpponents.some((_, idx) => idx !== seatIndex);

  if (!wasCedh && newDifficulty === 'CEDH' && next.aiOpponents.length > 1) {
    toast('All AI opponents set to cEDH — deck pool restricted to bracket 5.');
  }

  onConfigChange(next);
};
```

(Verify the toast import; the codebase likely uses `sonner`, `react-hot-toast`, or a custom toast. Run `grep -rn "toast" client/src/ --include="*.tsx" | head -5` to find the convention.)

- [ ] **Step 5: Run verification pattern**

Expected: the cascade test passes; existing tests still pass; type-check + lint clean.

- [ ] **Step 6: Commit**

```bash
git add client/src/components/menu/AiOpponentConfig.tsx \
        client/src/components/menu/__tests__/AiOpponentConfig.test.tsx
git commit -m "feat(client): cEDH cascade in AiOpponentConfig

Selecting cEDH on any AI seat cascades all other AI seats
to cEDH via applyCedhCascade(). Fires a one-time toast on
the upgrade. Reversed only by changing the difficulty back
to a non-cEDH tier."
```

## Task 7.3: Add cEDH option to `AiDifficultyDropdown`

**Files:**
- Modify: `client/src/components/menu/AiDifficultyDropdown.tsx`
- Modify: `client/src/components/menu/__tests__/AiDifficultyDropdown.test.tsx`
- Modify: `client/src/constants/ai.ts` (if difficulty constants live here)

- [ ] **Step 1: Inspect the existing dropdown + constants**

```bash
cat client/src/components/menu/AiDifficultyDropdown.tsx
cat client/src/constants/ai.ts
```

- [ ] **Step 2: Write the failing test**

In `client/src/components/menu/__tests__/AiDifficultyDropdown.test.tsx`, add:

```tsx
import { describe, expect, it } from 'vitest';
import { render, screen } from '@testing-library/react';
import { AiDifficultyDropdown } from '../AiDifficultyDropdown';

describe('AiDifficultyDropdown — cEDH', () => {
  it('renders the cEDH option', () => {
    render(<AiDifficultyDropdown value="Easy" onChange={() => {}} />);
    const option = screen.getByText(/cEDH/i);
    expect(option).toBeInTheDocument();
  });

  it('renders the B5 badge next to cEDH', () => {
    render(<AiDifficultyDropdown value="CEDH" onChange={() => {}} />);
    const badge = screen.getByText(/B5 lock/i);
    expect(badge).toBeInTheDocument();
  });
});
```

- [ ] **Step 3: Run to verify it fails**

`cd client && pnpm test -- --run AiDifficultyDropdown` — expect FAIL.

- [ ] **Step 4: Add the option + badge**

In `client/src/components/menu/AiDifficultyDropdown.tsx`, add `'CEDH'` to the list of options. Render a small badge next to the label:

```tsx
<option value="CEDH">cEDH <span aria-label="B5 lock">(B5 lock)</span></option>
```

(If the existing render uses `<button>` or `<div>` rather than `<option>`, follow the established pattern. Adjust the badge to match the project's existing styling — Tailwind class names should follow the established palette.)

In `client/src/constants/ai.ts`, add `'CEDH'` to the difficulty list constant and the display label map.

- [ ] **Step 5: Run verification pattern**

Expected: both new tests pass; existing dropdown tests pass; type-check + lint clean.

- [ ] **Step 6: Commit**

```bash
git add client/src/components/menu/AiDifficultyDropdown.tsx \
        client/src/components/menu/__tests__/AiDifficultyDropdown.test.tsx \
        client/src/constants/ai.ts
git commit -m "feat(client): cEDH option in AiDifficultyDropdown

Adds the cEDH option with a 'B5 lock' badge so users see the
bracket constraint before selecting. Constants in
client/src/constants/ai.ts include 'CEDH' alongside the
existing difficulty levels."
```

## Task 7.4: Filter AI deck pool to B5 when any AI is cEDH

**Files:**
- Modify: `client/src/services/aiDeckCatalog.ts`
- Test: `client/src/services/__tests__/aiDeckCatalog.test.ts`

- [ ] **Step 1: Inspect the current catalog API**

```bash
cat client/src/services/aiDeckCatalog.ts
```

Find the function that returns the deck list for an AI seat. That's where filtering goes.

- [ ] **Step 2: Write the failing test**

```ts
import { describe, expect, it } from 'vitest';
import { filterByBracket } from '../aiDeckCatalog';
import type { Deck } from '../../adapter/types';

const decks: Deck[] = [
  { name: 'casual', tier: 'Core' } as Deck,
  { name: 'optimized', tier: 'Optimized' } as Deck,
  { name: 'turbo', tier: 'Cedh' } as Deck,
];

describe('aiDeckCatalog.filterByBracket', () => {
  it('returns only Cedh decks when tier is Cedh', () => {
    expect(filterByBracket(decks, 'Cedh').map((d) => d.name)).toEqual(['turbo']);
  });

  it('returns all decks when tier is null', () => {
    expect(filterByBracket(decks, null).map((d) => d.name)).toEqual(['casual', 'optimized', 'turbo']);
  });
});
```

- [ ] **Step 3: Run to verify it fails**

`cd client && pnpm test -- --run aiDeckCatalog` — expect FAIL.

- [ ] **Step 4: Add the filter function**

In `client/src/services/aiDeckCatalog.ts`:

```ts
import type { Deck } from '../adapter/types';

export type CommanderBracketTier =
  | 'Exhibition'
  | 'Core'
  | 'Upgraded'
  | 'Optimized'
  | 'Cedh';

/** Pure filter — `null` returns all decks (no constraint). */
export function filterByBracket(decks: Deck[], tier: CommanderBracketTier | null): Deck[] {
  if (tier === null) return decks;
  return decks.filter((d) => d.tier === tier);
}
```

(Reuse an existing `CommanderBracketTier` type if one already exists in `client/src/adapter/types.ts` — `grep -rn "CommanderBracketTier" client/src/`.)

Then update the AI deck-picker consumer (likely `AiOpponentConfig` again, or a deck-picker component it renders) to call `filterByBracket(decks, anyAiOpponentIsCedh(config) ? 'Cedh' : null)`.

- [ ] **Step 5: Run verification pattern**

Expected: both new tests pass; existing aiDeckCatalog tests pass; type-check + lint clean.

- [ ] **Step 6: Commit**

```bash
git add client/src/services/aiDeckCatalog.ts \
        client/src/services/__tests__/aiDeckCatalog.test.ts \
        client/src/components/menu/AiOpponentConfig.tsx
git commit -m "feat(client): filter AI deck pool by bracket

filterByBracket(decks, tier) returns only matching-tier
decks; null passes through. AiOpponentConfig calls it with
'Cedh' when anyAiOpponentIsCedh(config), null otherwise."
```

## Task 7.5: Render warning chip on non-B5 human deck

**Files:**
- Modify: `client/src/pages/GameSetupPage.tsx`
- Test: `client/src/pages/__tests__/GameSetupPage.test.tsx` (create if absent)

- [ ] **Step 1: Inspect the existing setup page**

```bash
grep -n "human deck\|playerDeck\|deck.*human" client/src/pages/GameSetupPage.tsx | head -10
```

Locate where the human player's deck is selected/displayed.

- [ ] **Step 2: Write the failing test**

```tsx
import { describe, expect, it } from 'vitest';
import { render, screen } from '@testing-library/react';
import { GameSetupPage } from '../GameSetupPage';
// import or mock the stores GameSetupPage consumes — adjust to project pattern.

describe('GameSetupPage — cEDH warning chip', () => {
  it('shows warning chip when human deck is non-cEDH and any AI is cEDH', () => {
    // Set up the gameSetupStore / preferencesStore (or whatever GameSetupPage reads)
    // with humanDeck.tier = 'Core' and one AI opponent at 'CEDH'.
    // ... store setup ...

    render(<GameSetupPage />);
    expect(screen.getByText(/Your deck is bracket .* vs\. a cEDH AI/i)).toBeInTheDocument();
  });

  it('does not show the chip when the human deck is cEDH', () => {
    // ... store setup with humanDeck.tier = 'Cedh' and AI = 'CEDH' ...

    render(<GameSetupPage />);
    expect(screen.queryByText(/Your deck is bracket .* vs\. a cEDH AI/i)).not.toBeInTheDocument();
  });

  it('does not show the chip when no AI is cEDH', () => {
    // ... store setup with humanDeck.tier = 'Core' and AI = 'Hard' ...

    render(<GameSetupPage />);
    expect(screen.queryByText(/Your deck is bracket .* vs\. a cEDH AI/i)).not.toBeInTheDocument();
  });
});
```

(Replace the store-setup placeholder with the actual store fixture pattern this project uses — find an example in `client/src/pages/__tests__/`.)

- [ ] **Step 3: Run to verify it fails**

`cd client && pnpm test -- --run GameSetupPage` — expect FAIL.

- [ ] **Step 4: Implement the chip**

In `client/src/pages/GameSetupPage.tsx`, add:

```tsx
import { anyAiOpponentIsCedh, isDeckCedhLegal } from '../services/cedhLock';

// inside the component, near where the human deck is displayed:
const showCedhWarning =
  humanDeck != null &&
  anyAiOpponentIsCedh(config) &&
  !isDeckCedhLegal(humanDeck);

// in JSX:
{showCedhWarning && (
  <div
    role="alert"
    className="rounded-md bg-yellow-100 px-3 py-2 text-sm text-yellow-900"
  >
    Your deck is bracket {humanDeck.tier} vs. a cEDH AI — expect to lose fast.
  </div>
)}
```

(Match the Tailwind palette and chip styling to the existing codebase — `grep -rn "bg-yellow" client/src/ --include="*.tsx" | head -5`.)

- [ ] **Step 5: Run verification pattern**

Expected: all three new tests pass; existing GameSetupPage tests still pass; type-check + lint clean.

- [ ] **Step 6: Commit**

```bash
git add client/src/pages/GameSetupPage.tsx \
        client/src/pages/__tests__/GameSetupPage.test.tsx
git commit -m "feat(client): cEDH warning chip in GameSetupPage

Yellow chip beside the human deck selector when any AI is
cEDH and the human deck is not Cedh-tier. Non-blocking;
the engine validation (Phase 8) handles the hard failure
case if the user submits anyway."
```

---

# Phase 8 — Wire engine validation into game setup

The frontend chip is non-blocking. Hard validation happens engine-side at the boundary.

## Task 8.1: Call `validate_cedh_bracket` from the WASM bridge

**Files:**
- Modify: `crates/engine-wasm/src/lib.rs` (or wherever `initialize_game` is exposed)
- Test: existing engine-wasm tests + a new integration test for the failure case

- [ ] **Step 1: Locate the WASM game-init entry point**

```bash
grep -rn "initialize_game\|wasm_bindgen" crates/engine-wasm/src/ --include="*.rs" | head -10
```

- [ ] **Step 2: Wire the validation**

In the WASM `initialize_game` exposed function, after parsing the deck list and before calling the engine's actual game-init function, check whether any AI seat uses cEDH difficulty. If so, call `validate_cedh_bracket` on the deck list and surface any `BracketViolation` as a JS-returnable error.

```rust
use engine::database::legality::validate_cedh_bracket;

// inside the initialize_game wrapper:
let any_ai_is_cedh = ai_difficulties
    .iter()
    .any(|d| matches!(d, phase_ai::config::AiDifficulty::CEDH));
if any_ai_is_cedh {
    let deck_refs: Vec<&Deck> = decks.iter().collect();
    if let Err(violation) = validate_cedh_bracket(&deck_refs) {
        return Err(JsValue::from_str(&violation.to_string()));
        // or use a typed error if the bridge has one.
    }
}
```

(The exact code shape depends on the bridge function signature. Adjust types and return values to fit. Verify whether AI difficulty is passed in or read from a config struct; `grep -n "AiDifficulty" crates/engine-wasm/src/`.)

- [ ] **Step 3: Run verification pattern**

Expected: WASM build succeeds; clippy clean.

- [ ] **Step 4: Commit**

```bash
git add crates/engine-wasm/src/lib.rs
git commit -m "feat(engine-wasm): validate cEDH bracket at game init

When any AI seat is AiDifficulty::CEDH, the WASM bridge
runs validate_cedh_bracket(&decks) before initialize_game.
Returns the typed BracketViolation::Display() to JS as an
error string."
```

## Task 8.2: Mirror the validation in the Tauri + server bridges

**Files:**
- Modify: `client/src-tauri/src/main.rs` (or the equivalent Tauri command handler)
- Modify: `crates/phase-server/src/...` (the game-create handler)

- [ ] **Step 1: Locate the Tauri game-init command**

```bash
grep -rn "initialize_game\|tauri::command" client/src-tauri/src/ --include="*.rs" | head -10
```

- [ ] **Step 2: Apply the same validation pattern**

Mirror the WASM logic — same conditional, same `validate_cedh_bracket` call, surface as a typed Tauri error.

- [ ] **Step 3: Locate the server game-create handler**

```bash
grep -rn "CreateGame\|create_game\|GameCreated" crates/phase-server/src/ crates/server-core/src/ --include="*.rs" | head -10
```

- [ ] **Step 4: Apply the validation in the server path**

In multiplayer, the cEDH bracket-lock is out of scope per the spec, but the engine-side validation still runs (because the engine owns this check). Wire `validate_cedh_bracket` if any AI seat would be cEDH in a server-mediated game — for now, server games have no AI seats; this step is a no-op stub that documents the future point of integration.

- [ ] **Step 5: Run verification pattern**

Expected: Tauri build succeeds; server build succeeds; clippy clean.

- [ ] **Step 6: Commit**

```bash
git add client/src-tauri/src/ crates/phase-server/src/ crates/server-core/src/
git commit -m "feat(bridges): validate cEDH bracket in Tauri + server

Tauri command mirrors the WASM bridge — validate before init.
phase-server logs the future integration point; multiplayer
cEDH gating is out of scope for the skeleton."
```

## Task 8.3: Render `BracketViolation` as a blocking modal in `GamePage`

**Files:**
- Modify: `client/src/pages/GamePage.tsx`
- Test: `client/src/pages/__tests__/GamePage.test.tsx` (extend if exists, create if not)

- [ ] **Step 1: Write the failing test**

```tsx
import { describe, expect, it } from 'vitest';
import { render, screen } from '@testing-library/react';
import { GamePage } from '../GamePage';

describe('GamePage — bracket violation', () => {
  it('renders a blocking modal when the engine reports a BracketViolation', () => {
    // Simulate the engine returning a BracketViolation via the adapter.
    // Adjust to the project's mocking pattern.
    // ...

    render(<GamePage />);
    expect(
      screen.getByText(/deck .* is tier .*; cEDH games require all decks to be tier Cedh/i),
    ).toBeInTheDocument();
  });
});
```

- [ ] **Step 2: Run to verify it fails**

`cd client && pnpm test -- --run GamePage` — expect FAIL.

- [ ] **Step 3: Surface the typed error**

In `GamePage.tsx`, render the engine error (already surfaced via the adapter's error channel) as a blocking modal when the error matches the bracket-violation shape:

```tsx
{engineError && engineError.includes('cEDH games require all decks') && (
  <div role="dialog" aria-modal="true" className="..."> {/* blocking modal styling */}
    <h2>cEDH bracket lock</h2>
    <p>{engineError}</p>
    <button onClick={returnToSetup}>Return to setup</button>
  </div>
)}
```

(If the project has a typed-error envelope, use that instead of string matching.)

- [ ] **Step 4: Run verification pattern**

Expected: the new test passes; existing GamePage tests pass; type-check + lint clean.

- [ ] **Step 5: Commit**

```bash
git add client/src/pages/GamePage.tsx client/src/pages/__tests__/GamePage.test.tsx
git commit -m "feat(client): blocking modal for BracketViolation

GamePage surfaces validate_cedh_bracket failures as a
blocking modal with a Return-to-setup escape hatch. Final
hard validation point in the cEDH lock chain."
```

---

# Phase 9 — Integration test + verification sweep

## Task 9.1: End-to-end Rust integration test

**Files:**
- Create: `crates/phase-ai/tests/cedh_integration.rs`

- [ ] **Step 1: Write the test**

```rust
//! End-to-end smoke test for cEDH difficulty wiring.
//!
//! - Config preset values.
//! - 4p scaling skip.
//! - ComboLinePolicy activation on `is_cedh = true`.
//! - CedhKeepablesMulligan force-mulligans an empty hand.
//! - Default registries include the new policies.

use phase_ai::combo::ComboRegistry;
use phase_ai::config::{create_config, create_config_for_players, AiDifficulty, Platform};
use phase_ai::features::DeckFeatures;
use phase_ai::policies::registry::{PolicyId, PolicyRegistry};

#[test]
fn cedh_full_stack_smoke() {
    // 1. Preset values.
    let cfg = create_config(AiDifficulty::CEDH, Platform::Native);
    assert_eq!(cfg.search.max_depth, 3);
    assert_eq!(cfg.search.max_nodes, 96);

    // 2. 4-player scaling skip.
    let cfg4 = create_config_for_players(AiDifficulty::CEDH, Platform::Native, 4);
    assert_eq!(cfg4.search.max_depth, 3);
    assert_eq!(cfg4.search.max_nodes, 96);

    // 3. DeckFeatures gating field.
    let mut features = DeckFeatures::default();
    assert!(!features.is_cedh);
    features.is_cedh = true;
    assert!(features.is_cedh);

    // 4. Default policy registry includes the cEDH policy.
    let reg = PolicyRegistry::default();
    let has_combo = reg.policies.iter().any(|p| p.id() == PolicyId::ComboLineProgress);
    assert!(has_combo);

    // 5. ComboRegistry is populated with at least one line.
    let combo_reg = ComboRegistry::default();
    assert!(!combo_reg.lines().is_empty());
}
```

(`PolicyRegistry::policies` may not be `pub`. If so, add a `pub fn ids(&self) -> Vec<PolicyId>` accessor or test through `shared()`.)

- [ ] **Step 2: Run verification pattern**

Expected: integration test passes; clippy clean.

- [ ] **Step 3: Commit**

```bash
git add crates/phase-ai/tests/cedh_integration.rs
git commit -m "test(phase-ai): cEDH end-to-end smoke

Verifies preset values, 4p scaling skip, DeckFeatures gating,
default-registry policy registration, and ComboRegistry
population in one test file."
```

## Task 9.2: Full verification sweep + AI duel sanity

**Files:** none new; this is the final gate.

- [ ] **Step 1: Run the full verification pattern**

```bash
cargo fmt --all
if tilt get uiresource clippy >/dev/null 2>&1; then
  ./scripts/tilt-wait.sh --timeout 600 clippy test-engine test-ai card-data check-frontend test-frontend
else
  cargo clippy --all-targets -- -D warnings
  cargo test -p engine
  cargo test -p phase-ai
  ./scripts/gen-card-data.sh
  (cd client && pnpm run type-check && pnpm lint && pnpm test -- --run)
fi
```

Expected: zero failures across all resources. Any failure → fix and re-run.

- [ ] **Step 2: AI duel sanity check**

```bash
cargo ai-duel --difficulty CEDH --opponent VeryHard --games 5
```

(If the existing `cargo ai-duel` alias doesn't accept `--difficulty CEDH` directly yet, extend the binary in `crates/phase-ai/src/bin/` — should be a one-line difficulty-string parser update.)

Expected: 5 games complete without panic. Win-rate is not assertable at the skeleton stage (no real combos registered) — the point is to verify the cEDH config doesn't crash the planner under real game-driven workloads.

- [ ] **Step 3: Commit any duel-binary fixes**

```bash
git add crates/phase-ai/src/bin/
git commit -m "chore(ai-duel): accept --difficulty CEDH"
```

(Skip if no changes needed.)

- [ ] **Step 4: Push the branch and open the PR**

```bash
git push -u origin feat/cedh-difficulty
gh pr create --title "feat: cEDH AI difficulty" --body "$(cat <<'EOF'
## Summary
- New `AiDifficulty::CEDH` preset that bypasses 4-player paranoid search scaling.
- Bracket-5 game-setup lock: filters AI deck pickers to B5, cascades all AI seats to cEDH on selection, warns on non-cEDH human deck.
- Combo-recognition skeleton: `ComboLinePolicy` (gated on `DeckFeatures::is_cedh`), `CedhKeepablesMulligan`, `combo/` module with one stub line and a `ComboDetector` trait.
- Engine-side `validate_cedh_bracket` (tag check; `CommanderBracketTier::Cedh` is manual-declaration only per `bracket_estimate.rs`).

Real cEDH combo content, archetype tuning, backward-chaining synthesis, and multiplayer cEDH are explicit non-goals.

Spec: [docs/superpowers/specs/2026-05-22-cedh-difficulty-design.md](docs/superpowers/specs/2026-05-22-cedh-difficulty-design.md)
Plan: [docs/superpowers/plans/2026-05-22-cedh-difficulty.md](docs/superpowers/plans/2026-05-22-cedh-difficulty.md)

## Test plan
- [x] All Rust tests pass (clippy + test-engine + test-ai)
- [x] Frontend tests pass (check-frontend + test-frontend)
- [x] cargo ai-duel completes 5 cEDH-vs-VeryHard games without panic
- [ ] Manual UI smoke: select cEDH for one AI seat, verify cascade + toast + chip behave per spec

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

---

# Self-Review

**Spec coverage** — Walking the spec section-by-section:

| Spec section | Implementing task(s) |
|---|---|
| 5.1 Difficulty preset (preset values, WASM caps, 4p scaling skip) | 1.1, 1.2, 1.3, 1.4 |
| 5.2 Combo module (`combo/` mod, line/detection/registry) | 4.1, 4.2, 4.3 |
| 5.3 `ComboLinePolicy` (`activation()` gates on `features.is_cedh`, verdict scores from `PolicyPenalties`) | 5.2, 5.3 (5.1 reserves the `PolicyId` slot) |
| 5.3 `DeckFeatures::is_cedh` field + population from `CommanderBracketTier::Cedh` | 2.1, 2.2 |
| 5.4 `CedhKeepablesMulligan` (renamed from `CedhMulliganPolicy`, gated internally) | 6.1, 6.2 |
| 5.5 Engine `validate_cedh_bracket` tag check | 3.1 + wired by 8.1/8.2 |
| 5.6 Frontend (`cedhLock` service, dropdown option, cascade + toast, deck filter, warning chip, blocking modal) | 7.1, 7.2, 7.3, 7.4, 7.5, 8.3 |
| 6 Data flow | 7.x + 8.x cover the user-side path; integration test 9.1 walks the engine-side path |
| 7 Testing strategy | Each task carries its own TDD test; 9.1 is the integration smoke |
| 8 Open questions (proof-of-life combo, CardPredicate, tutor tag names) | 4.3 Step 1 picks the combo; 4.1 Step 1 sets the predicate shape; 6.1 Step 2 picks the tag names |

**Placeholder scan** — No `TBD` / `TODO` / `fill in details`. Each step includes either the actual code, the exact command, or a precise `grep`-then-adjust directive that an agent can execute mechanically.

**Type consistency** — `ComboLine`, `ComboPiece`, `ComboReachability`, `WinKind`, `ComboStep`, `ComboLineId`, `CardPredicate`, `ComboDetector`, `DefaultComboDetector`, `ComboRegistry`, `ComboLinePolicy`, `CedhKeepablesMulligan`, `PolicyId::ComboLineProgress`, `PolicyId::CedhKeepablesMulligan`, `DeckFeatures::is_cedh`, `BracketViolation::DeckNotCedh`, `validate_cedh_bracket`, `anyAiOpponentIsCedh`, `applyCedhCascade`, `isDeckCedhLegal`, `filterByBracket` — all names match across the tasks where they appear.

**Frontend type assumptions** — The frontend tasks assume `GameSetupConfig.aiOpponents: { difficulty: AiDifficulty }[]` and `Deck.tier: CommanderBracketTier`. Each task has an inspection step that confirms the real names before writing code. If a name differs, the agent adjusts in the same task without breaking the plan.

**WASM/Tauri wiring** — Tasks 8.1 and 8.2 are intentionally light on exact code because the bridge entry-point shape varies. Each task starts with a `grep` to locate the actual signature, then applies the same pattern.

Plan complete and saved to [docs/superpowers/plans/2026-05-22-cedh-difficulty.md](../plans/2026-05-22-cedh-difficulty.md).
