---
name: ai-duel
description: Use when running AI-vs-AI duel simulations, validating AI matchup quality, checking combat or spellcasting regressions, tuning ai-duel CLI options, or interpreting batch simulation results.
---

# AI Duel Simulation

Run AI-vs-AI game simulations to test decision quality, validate matchups, and catch regressions.

## Quick Start

```bash
# Default: Red Aggro vs Green Midrange, 5 games, Medium difficulty
rtk cargo run --release --bin ai-duel -- client/public --batch 5

# Single verbose game (see every combat action and spell cast)
rtk cargo run --release --bin ai-duel -- client/public --seed 42 --difficulty VeryHard

# Batch with specific seed for reproducibility
rtk cargo run --release --bin ai-duel -- client/public --batch 20 --seed 1000 --difficulty Medium

# Full registered matchup suite in measurement mode
rtk cargo run --release --bin ai-duel -- client/public --suite --games 10 --seed 42 \
  --output target/duel-suite-results.json

# Compare two suite reports with paired-seed sign-test status
rtk cargo run --release --bin ai-duel -- compare crates/phase-ai/baselines/duel-suite.json \
  target/duel-suite-results.json

# Commander candidate-seat measurement
rtk cargo run --release --bin ai-duel -- client/public --commander-suite --games 8 --seed 42 \
  --difficulty Hard --baseline-difficulty Medium \
  --output target/commander-suite-results.json
```

## CLI Options

| Flag | Description | Default |
|------|-------------|---------|
| `--batch N` | Run N games, print summary only | 1 (verbose) |
| `--seed S` | RNG seed for reproducibility | time-based |
| `--difficulty LEVEL` | `VeryEasy\|Easy\|Medium\|Hard\|VeryHard` | Medium |
| `--matchup NAME` | Deck matchup preset | red-vs-green |
| `--list-matchups` | Show available matchups | - |
| `--verbose` | Print every action (full trace) | off |
| `--suite` | Run every registered `MatchupSpec` in measurement mode | off |
| `--games N` | Games per matchup/suite cell | 10 for suite, 4 for commander suite |
| `--output PATH` | JSON report path | `target/duel-suite-results.json` |
| `--suite-filter STR` | Run only suite matchups whose id contains STR | all |
| `--show-attribution` | Capture `phase_ai::decision_trace` policy attribution | off |
| `--commander-suite` | Run 4-player Commander candidate-seat rotations | off |
| `--baseline-difficulty LEVEL` | Baseline seats for `--commander-suite` | Medium |
| `--feed PATH` | Commander feed under data root | `feeds/mtggoldfish-commander.json` |

## Measurement And Gates

`ai-duel` uses `AiConfig::into_measurement(seed)` for single, suite, and
Commander regression runs. Measurement mode disables wall-clock search budgets
and bounds search by node/depth budgets, so outcomes are functions of the
binary, config, matchup, and seed.

Use `rtk cargo ai-gate` for the normal regression gate. It runs the pinned suite
against `crates/phase-ai/baselines/`, compares paired seeds, and reports
FAIL/WARN/PASS with a binomial sign-test p-value. WARN means movement was
observed but not significant enough to fail the gate.

When intentionally changing AI behavior, refresh the baseline in the same PR
after review:

```bash
rtk ./scripts/refresh-ai-baseline.sh
rtk cargo ai-gate
```

**Exception — deadline / wall-clock-budget changes are inert under the gate; do
NOT refresh the baseline for them.** Because measurement mode nulls the
wall-clock budget (`PlannerServices::with_deadline` forces `Deadline::none()`
whenever `execution_mode.is_measurement()`, and `Deadline::none().expired()` is
always `false`), any code guarded by `self.deadline.expired()` is a dead branch
during `ai-gate`, so the run is byte-identical and **zero baseline movement is
the *expected* result** — not evidence the change did nothing. If `ai-gate`
diverges after a deadline-only change, that is a bug (a guard fired on a live
path), not a baseline event: fix the guard, don't refresh baselines to silence
it. Unit-test such guards by injecting
`with_deadline(..., Some(Deadline::after(0)))` on a **non-measurement** config —
a `.into_measurement()` config nulls the injected deadline, so the guard never
fires and the test passes vacuously — and assert `services.deadline.expired()`
as a witness before the negative assertion. Example: PR #6255 bounded the
`quiesce` / `sample_backfilled_continuations` search apply-loops this way with
no baseline change.

## Performance Guide

All times are release mode (`--release`). Debug mode is 5-10x slower.

| Difficulty | Time/Game | Search | Use Case |
|-----------|-----------|--------|----------|
| VeryEasy | ~1s | None (random) | Stress testing |
| Easy | ~3s | None (heuristic) | Baseline sanity |
| Medium | ~24s | Depth 2, 24 nodes | **Primary testing** |
| Hard | ~60s | Depth 3, 48 nodes | Quality validation |
| VeryHard | ~126s | Depth 3, 64 nodes | Final verification |

## Deck Configuration

The `ai-duel` binary resolves `--matchup` and `--suite` entries from
`crates/phase-ai/src/duel_suite/spec.rs`. Inline starter/metagame deck builders
live in `crates/phase-ai/src/duel_suite/inline_decks.rs`; snapshot decks live
under `crates/phase-ai/duel_decks/`.

### Available Matchups

Use `--matchup NAME` to select a preset. Use `--list-matchups` to see all options.

**Starter decks** (mono-colored, simple cards for baseline testing):

| Matchup | P0 | P1 |
|---------|----|----|
| `red-vs-green` (default) | Red Aggro | Green Midrange |
| `blue-vs-green` | Blue Control | Green Midrange |
| `red-vs-blue` | Red Aggro | Blue Control |
| `black-vs-green` | Black Midrange | Green Midrange |
| `white-vs-red` | White Weenie | Red Aggro |
| `black-vs-blue` | Black Midrange | Blue Control |
| `red-mirror` | Red Aggro | Red Aggro |
| `green-mirror` | Green Midrange | Green Midrange |
| `blue-mirror` | Blue Control | Blue Control |

**Metagame decks** (real competitive lists from MTGGoldfish feeds, 100% engine coverage):

| Matchup | P0 | P1 | Tests |
|---------|----|----|-------|
| `azorius-vs-prowess` | Pioneer Azorius Control | Mono-Red Prowess | Aggro vs control |
| `azorius-vs-gruul` | Pioneer Azorius Control | Gruul Prowess | Control vs aggro variant |
| `delver-vs-prowess` | Legacy Izzet Delver | Mono-Red Prowess | Tempo vs aggro |
| `azorius-vs-green` | Pioneer Azorius Control | Green Midrange | **Control vs midrange** |
| `delver-vs-green` | Legacy Izzet Delver | Green Midrange | Tempo vs midrange |
| `prowess-vs-green` | Mono-Red Prowess | Green Midrange | Aggro vs midrange |
| `prowess-mirror` | Mono-Red Prowess | Mono-Red Prowess | Mirror match |

### Changing Decks

To add new matchups, add a `MatchupSpec` in `duel_suite/spec.rs`. Use an inline
builder only for small stable decks; prefer snapshot decks for real metagame
coverage. Card names must match entries in `client/public/card-data.json`. Use
`jq 'keys[]' client/public/card-data.json | rg -i "card name"` to find exact names.

To find high-coverage metagame decks for testing, check the feed data:
```bash
# List all feeds
ls client/public/feeds/

# Check a deck's card coverage against the engine
python3 -c "
import json
with open('client/public/card-data.json') as f:
    db = {k.lower(): v for k, v in json.load(f).items()}
with open('client/public/feeds/mtggoldfish-pioneer.json') as f:
    feed = json.load(f)
for deck in feed['decks']:
    sup = sum(1 for e in deck['main'] if e['name'].lower() in db)
    print(f'{sup}/{len(deck[\"main\"])} {deck[\"name\"]}')
"
```

### Matchup Triangle (Expected Results)

The classic archetype triangle should hold:
- **Aggro > Control** — kills before control stabilizes
- **Control > Midrange** — removal + card draw outgrinds
- **Midrange > Aggro** — bigger creatures brick aggro attacks

Control decks improve more at higher difficulty levels (they need search to time removal correctly).

### Mirror Match Testing

For testing AI quality independent of deck matchup advantage, use mirror matches (`prowess-mirror`, `red-mirror`, etc.). Win rates should be close to 50/50.

### Commander Suite

`--commander-suite` loads four resolvable decks from the Commander feed, runs
one candidate seat against three baseline seats, and rotates the candidate seat
across P0-P3. Metrics are candidate win rate, survival turns, and elimination
order. Use this as the first strength signal for multiplayer/Commander changes;
it is slower than the two-player suite and should run nightly or on targeted AI
changes rather than on every parser-only change.

## Interpreting Results

**Healthy signs:**
- 0 draws/aborted games
- Games complete in 10-20 turns
- Win rates match expected archetype matchups
- Higher difficulty = longer games (smarter defensive play)

**Warning signs:**
- Any draws/aborted games → AI might be stuck in a loop
- Games > 30 turns → AI might not be attacking efficiently
- Same player always wins regardless of seed → deck balance issue
- Higher difficulty = worse results → search/evaluation regression

## Verbose Output Patterns to Watch

When running single verbose games, look for:

- **Self-targeting**: "X deals N damage to X" — anti-self-harm policy failure
- **Wasteful spells**: Combat tricks cast outside combat, counterspells with empty stack
- **Suicidal blocking**: Blocking at low life when the block damage kills you
- **Not attacking with lethal**: Having lethal on board but not swinging
- **Tapping out into lethal**: Casting sorcery-speed when opponent has lethal on board

## Community Scenario Fixtures

When a Discord `#ai-suggestions` thread includes a saved game-state zip, turn it
into a deterministic AI scenario before landing the fix:

1. Download the zip and commit it under `crates/phase-ai/fixtures/scenarios/`.
2. Add an entry to `crates/phase-ai/fixtures/scenarios/community-scenarios.json`
   with the Discord thread id, archive name, and expected action assertion.
3. Run the fixture through `crates/phase-ai/tests/community_scenarios.rs`; it
   loads the same `{ "gameState": ... }` export used by `ai-bench-state`, applies
   saved-state compatibility migrations, and evaluates the action in measurement
   mode with a fixed seed.

## Related Files

| File | Purpose |
|------|---------|
| `crates/phase-ai/src/bin/ai_duel.rs` | Duel simulation binary |
| `crates/phase-ai/src/bin/ai_tune.rs` | CMA-ES weight optimization |
| `crates/phase-ai/src/bin/ai_gate.rs` | Pinned regression gate wrapper |
| `crates/phase-ai/src/auto_play.rs` | AI action driver |
| `crates/phase-ai/src/combat_ai.rs` | Combat decisions |
| `crates/phase-ai/src/duel_suite/` | Matchup registry, runner, compare, attribution |
| `crates/phase-ai/src/search.rs` | Action selection + search |
| `crates/phase-ai/tests/ai_quality.rs` | Regression test suite |
| `crates/phase-ai/tests/scenarios.rs` | Scenario integration tests |
| `crates/phase-ai/tests/community_scenarios.rs` | Discord saved-state scenario tests |
