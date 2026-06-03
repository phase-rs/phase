# Phase 1 — Coverage inventory (mega-features roadmap)

Generated from `cargo run --quiet --bin coverage-report -- data` (34,771 cards, **86.89%** supported).

## Target set

`client/public/set-list.json` is not in-repo; triage used **format-weighted corpus** instead. **Standard** legal cards: 4,310 / 4,649 (**92.71%**). Work proceeds corpus-wide via gap clusters (unlock-set style), not a single MTGJSON set export.

## Top single-gap handlers (`top_gaps`)

| Handler | Single-gap cards |
|---------|------------------|
| `Effect:unknown` | 420 |
| `hidden_gap` | 459 |
| `Effect:static_structure` | 254 |
| `Effect:effect_structure` | (see full report) |

Top `Effect:unknown` oracle patterns: `specialize {N}` (15), conspiracy/draft-face-up lines, `starting intensity N`, augment, Runner mechanics.

## Highest-ROI 2-gap bundle (cluster #1)

**Handlers:** `Effect:static_structure` + `Effect:unknown`  
**Unlock:** 35 cards (Commander-heavy)

## Cluster #2 (iterate)

**Handlers:** `Effect:get` + `Effect:put` — next ranked 2-gap bundle in `gap_bundles`.

## Process

Each cluster: engine-implementer (plan → review → implement → review). Tilt-first verification per CONTRIBUTING.
