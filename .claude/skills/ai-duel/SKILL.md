---
name: ai-duel
description: Use when running AI-vs-AI duel simulations, validating AI matchup quality, checking combat or spellcasting regressions, tuning ai-duel CLI options, or interpreting batch simulation results.
---

# AI Duel Simulation

Run AI-vs-AI game simulations to test decision quality, validate matchups, and catch regressions.

## Quick Start

```bash
# Default: Red Aggro vs Green Midrange, 5 games, Medium difficulty
cargo run --release --bin ai-duel -- client/public --batch 5

# Single verbose game (see every combat action and spell cast)
cargo run --release --bin ai-duel -- client/public --seed 42 --difficulty VeryHard

# Batch with specific seed for reproducibility
cargo run --release --bin ai-duel -- client/public --batch 20 --seed 1000 --difficulty Medium
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

The `ai-duel` binary uses hardcoded decks selected via `--matchup`. Deck functions are in `crates/phase-ai/src/bin/ai_duel.rs`.

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

To add new matchups, add a deck builder function and matchup entry in `ai_duel.rs`. Card names must match entries in `client/public/card-data.json`. Use `jq 'keys[]' client/public/card-data.json | grep -i "card name"` to find exact names.

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

## Related Files

| File | Purpose |
|------|---------|
| `crates/phase-ai/src/bin/ai_duel.rs` | Duel simulation binary |
| `crates/phase-ai/src/bin/ai_tune.rs` | CMA-ES weight optimization |
| `crates/phase-ai/src/auto_play.rs` | AI action driver |
| `crates/phase-ai/src/combat_ai.rs` | Combat decisions |
| `crates/phase-ai/src/search.rs` | Action selection + search |
| `crates/phase-ai/tests/ai_quality.rs` | Regression test suite |
| `crates/phase-ai/tests/scenarios.rs` | Scenario integration tests |
