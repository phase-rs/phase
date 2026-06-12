---
name: retrain-ai-weights
description: Use when retraining AI evaluation weights from 17Lands replay data, adding new training datasets, updating learned weight values in phase-ai Rust code, or running CMA-ES optimization for AI profiles and evaluation parameters.
---

# Retrain AI Evaluation Weights

Use when the user wants to retrain AI weights from 17Lands data, add new training datasets, update learned weight values in Rust, or run CMA-ES optimization.

## Architecture Overview

The AI weight system has 4 layers:

1. **Base weights** (`EvalWeightSet` in `crates/phase-ai/src/eval.rs`) — 9 weights × 3 game phases (early T1-3, mid T4-7, late T8+). Learned from 17Lands replay data.
2. **Archetype multipliers** (`ArchetypeMultipliers` in `crates/phase-ai/src/deck_profile.rs`) — 5 archetypes × 9 multipliers. Scale base weights per deck type.
3. **Keyword bonuses** (`KeywordBonuses` in `crates/phase-ai/src/eval.rs`) — 10 params for creature evaluation.
4. **Policy penalties** (`PolicyPenalties` in `crates/phase-ai/src/config.rs`) — tactical policy score knobs.
5. **AiProfile** (`crates/phase-ai/src/config.rs`) — 3 params (risk_tolerance, interaction_patience, stabilize_bias).

All stored in `AiConfig`. The CMA-ES optimizer tunes one parameter group per
run via `--group eval|penalties|keywords|archetype`:

- `eval`: 9 late-game `EvalWeights` plus 3 `AiProfile` values. Early/mid weights are derived from the 17Lands phase ratios.
- `penalties`: every field listed in `ACTIVE_POLICY_PENALTY_FIELDS`.
- `keywords`: all `KeywordBonuses` fields.
- `archetype`: 5 archetypes x 9 `ArchetypeMultipliers`.

Do not mix groups in one run. Compare and validate one group artifact at a
time so regressions can be attributed to a specific surface.

## Training Data Setup

**Data location:** `data/17lands/` (gitignored)

**Required files from 17Lands (https://www.17lands.com/public_datasets):**
- `replay_data_public.{SET}.PremierDraft.csv` — Per-turn board state snapshots. Premier Draft (Bo1) is best: largest dataset, no sideboard confounds, human-drafted decks.
- `cards.csv` — Arena card ID to mana value mapping.

**To add new sets:** Download CSVs and symlink or copy into `data/17lands/`:
```bash
ln -s ~/Downloads/replay_data_public.FDN.PremierDraft.csv data/17lands/
ln -s ~/Downloads/replay_data_public.DSK.PremierDraft.csv data/17lands/
# cards.csv only needed once (shared across sets)
ln -s ~/Downloads/cards.csv data/17lands/
```

The script auto-discovers all `replay_data_public.*.PremierDraft.csv` files.

## Retraining Steps

### Step 1: Run the training script

```bash
rtk python3 scripts/train_eval_weights.py --data-dir data/17lands --output data/learned-weights.json
```

**Dependencies:** `pip3 install -r scripts/requirements-training.txt` (pandas, scikit-learn, numpy)

**What it does:**
- Streams all replay CSVs with skill filter (win_rate >= 0.55, games >= 50)
- Splits samples into 3 turn-phase buckets (early T1-3, mid T4-7, late T8+)
- Trains separate logistic regression per phase
- Maps 5 features to EvalWeights fields: life_diff→life, creature_count_diff→board_presence, creature_mv_diff→board_power, hand_diff→hand_size, non_creature_diff→card_advantage
- Scales so max coefficient = 2.5
- 4 weights stay hand-tuned: board_toughness=1.0, aggression=0.5, zone_quality=0.3, synergy=0.5

**Output:** `data/learned-weights.json` with per-phase weights and accuracy metrics.

### Step 2: Update Rust with new values

Read `data/learned-weights.json` and update `EvalWeightSet::learned()` in `crates/phase-ai/src/eval.rs`:

```rust
pub fn learned() -> Self {
    EvalWeightSet {
        early: EvalWeights {
            life: /* phases.early.weights.life */,
            aggression: /* phases.early.weights.aggression */,
            board_presence: /* phases.early.weights.board_presence */,
            board_power: /* phases.early.weights.board_power */,
            board_toughness: /* phases.early.weights.board_toughness */,
            hand_size: /* phases.early.weights.hand_size */,
            zone_quality: /* phases.early.weights.zone_quality */,
            card_advantage: /* phases.early.weights.card_advantage */,
            synergy: /* phases.early.weights.synergy */,
        },
        mid: EvalWeights { /* same pattern from phases.mid.weights */ },
        late: EvalWeights { /* same pattern from phases.late.weights */ },
    }
}
```

### Step 3: Verify

```bash
rtk cargo fmt --all
rtk ./scripts/tilt-wait.sh --timeout 420 clippy test-ai
```

If Tilt is not running (`rtk tilt get uiresource clippy` fails), use the
project-reference skill before falling back to direct cargo commands.

### Step 4 (optional): CMA-ES optimization

```bash
# Smoke test (fast, verifies binary works)
rtk cargo tune-ai data/ --group eval --generations 2 --population 5 --games 3 --seed 42

# Full eval/profile run
rtk cargo tune-ai data/ --group eval --generations 100 --population 50 --games 20 --output data/cma-tuned-eval.json

# Full policy-penalty run
rtk cargo tune-ai data/ --group penalties --generations 100 --population 50 --games 20 --output data/cma-tuned-penalties.json

# Full keyword-bonus run
rtk cargo tune-ai data/ --group keywords --generations 100 --population 50 --games 20 --output data/cma-tuned-keywords.json

# Full archetype-multiplier run
rtk cargo tune-ai data/ --group archetype --generations 100 --population 50 --games 20 --output data/cma-tuned-archetype.json

# Validate a tuned artifact against paired holdout matchups and Easy/Medium/Hard opponents
rtk cargo tune-ai data/ --validate --games 500 --output data/cma-tuned-eval.json
```

CMA-ES writes the requested artifact plus a sibling `*-manifest.json`
containing the git SHA, seed, group, parameter names, fitness decks, holdout
decks, opponent pool, games/eval, paired-seed flag, draw-exclusion flag, and
baseline config hash. Do not write CMA output to `data/learned-weights.json`;
that path is reserved for the 17Lands logistic-regression artifact
(`kind: 17lands_phase_weights`). After a full run, review the validation
output before manually updating Rust defaults.

Fitness uses registered `duel_suite` matchup IDs from the fitness split and
paired mirrored seeds. Drawn games are excluded from the fitness denominator.
Holdout validation uses a separate registered split and compares baseline vs
learned on the same seeds against Easy, Medium, and Hard opponent configs.

Commander sanity measurement lives in `ai-duel`, not `ai-tune`:

```bash
rtk cargo run --release --bin ai-duel -- client/public --commander-suite --games 8 --seed 42 \
  --difficulty Hard --baseline-difficulty Medium \
  --output target/commander-suite-results.json
```

This runs the candidate seat through four seat rotations against three
baseline seats and reports win rate, survival turns, and elimination order.

## Key Files

| File | Purpose |
|------|---------|
| `scripts/train_eval_weights.py` | Python training pipeline |
| `scripts/requirements-training.txt` | Python deps (pandas, scikit-learn, numpy) |
| `data/learned-weights.json` | Trained weight artifact (committed) |
| `data/cma-tuned-weights.json` | CMA-ES tuned artifact (generated, not a 17Lands artifact) |
| `data/cma-tuned-weights-manifest.json` | CMA-ES reproducibility manifest |
| `data/17lands/` | Raw 17Lands CSVs (gitignored) |
| `crates/phase-ai/src/eval.rs` | `EvalWeights`, `EvalWeightSet`, `KeywordBonuses`, evaluation functions |
| `crates/phase-ai/src/deck_profile.rs` | `ArchetypeMultipliers`, deck classification |
| `crates/phase-ai/src/config.rs` | `AiConfig` with all tunable params |
| `crates/phase-ai/src/bin/ai_tune.rs` | CMA-ES optimizer binary |
| `crates/phase-ai/src/bin/ai_duel.rs` | Duel-suite, compare, and Commander measurement binary |

## EvalWeights Fields (9 total)

| Field | 17Lands Feature | Measures |
|-------|----------------|----------|
| `life` | life_diff | Life total differential |
| `board_presence` | creature_count_diff | Creature count differential |
| `board_power` | creature_mv_diff | Total mana value of creatures |
| `hand_size` | hand_diff | Cards in hand differential |
| `card_advantage` | non_creature_diff | Non-creature, non-land permanents |
| `board_toughness` | — | Total toughness (hand-tuned) |
| `aggression` | — | Power bonus when ahead on life (hand-tuned) |
| `zone_quality` | — | Hand quality + graveyard value (hand-tuned) |
| `synergy` | — | Board synergy bonus (hand-tuned) |
