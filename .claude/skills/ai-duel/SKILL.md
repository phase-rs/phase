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

The `ai-duel` binary uses hardcoded decks in `build_starter_decks()` at `crates/phase-ai/src/bin/ai_duel.rs:264`.

### Current Decks

**Player 0 — Red Aggro** (20 lands, 40 spells):
Mountains, Goblin Guide, Monastery Swiftspear, Raging Goblin, Jackal Pup, Mogg Fanatic, Lightning Bolt, Shock, Lava Spike, Searing Spear, Skullcrack

**Player 1 — Green Midrange** (22 lands, 38 spells):
Forests, Llanowar Elves, Elvish Mystic, Grizzly Bears, Kalonian Tusker, Centaur Courser, Leatherback Baloth, Giant Growth, Rancor, Titanic Growth, Rabid Bite

### Changing Decks

To test different matchups, modify `build_starter_decks()` in `ai_duel.rs`. Card names must match entries in `client/public/card-data.json`. Use `jq 'keys[]' client/public/card-data.json | grep -i "card name"` to find exact names.

Example deck configurations for testing:

**Blue Control** (for control vs midrange):
```rust
let mut blue = Vec::with_capacity(60);
blue.extend(repeat("Island", 24));
blue.extend(repeat("Counterspell", 4));
blue.extend(repeat("Mana Leak", 4));
blue.extend(repeat("Unsummon", 4));
blue.extend(repeat("Divination", 4));
blue.extend(repeat("Air Elemental", 4));
blue.extend(repeat("Frost Titan", 2));
blue.extend(repeat("Think Twice", 4));
blue.extend(repeat("Cancel", 4));
blue.extend(repeat("Aether Gust", 2));
blue.extend(repeat("Negate", 4));
```

**White Weenie** (for aggro mirror):
```rust
let mut white = Vec::with_capacity(60);
white.extend(repeat("Plains", 20));
white.extend(repeat("Savannah Lions", 4));
white.extend(repeat("Elite Vanguard", 4));
white.extend(repeat("Raise the Alarm", 4));
white.extend(repeat("Glorious Anthem", 4));
white.extend(repeat("Swords to Plowshares", 4));
white.extend(repeat("Serra Angel", 4));
white.extend(repeat("Oblivion Ring", 4));
white.extend(repeat("Disenchant", 4));
white.extend(repeat("Honor of the Pure", 4));
white.extend(repeat("Militia Bugler", 4));
```

### Matchup Triangle (Expected Results)

The classic archetype triangle should hold:
- **Aggro > Control** — kills before control stabilizes
- **Control > Midrange** — removal + card draw outgrinds
- **Midrange > Aggro** — bigger creatures brick aggro attacks

If simulation results don't match this pattern, the AI has a decision quality issue.

### Same-Archetype Testing

For testing AI quality independent of deck matchup advantage, use **mirror matches** — same deck on both sides. Modify `build_starter_decks()` to use the same card list for both `player` and `opponent`. Win rates should be close to 50/50 in mirror matches.

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
