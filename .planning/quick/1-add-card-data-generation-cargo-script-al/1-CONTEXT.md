# Quick Task 1: Add card-data generation, cargo script aliases, and README - Context

**Gathered:** 2026-03-08
**Status:** Ready for planning

<domain>
## Task Boundary

Add card-data generation pipeline, expand cargo aliases as task runner, create README, and ensure full first-run prerequisites are covered (git clone → running app).

</domain>

<decisions>
## Implementation Decisions

### Card Data Generation
- Add a script that downloads Forge's cardsfolder from GitHub, then runs `card_data_export` to generate `client/public/card-data.json`
- The `card_data_export` binary already exists at `crates/engine/src/bin/card_data_export.rs`

### Task Runner
- Expand `.cargo/config.toml` with more cargo aliases (e.g., `cargo gen-cards`, `cargo serve`)
- Zero dependencies, idiomatic Rust approach
- Existing aliases: `wasm`, `wasm-release`

### README
- Both user-facing showcase (features, how to play) AND developer docs (architecture, build, contribute)
- User-facing intro up top, developer docs below

### Prerequisites / Setup
- Full "git clone to running" path: card data generation + WASM build check + pnpm install + env setup
- Consider a single setup script or clear cargo alias that orchestrates everything

### Claude's Discretion
- Specific Forge GitHub download URL/method
- README formatting and section ordering
- Script implementation language (bash vs rust)

</decisions>

<specifics>
## Specific Ideas

- Existing build script: `scripts/build-wasm.sh` (handles WASM pipeline)
- Existing cargo aliases in `.cargo/config.toml`: `wasm`, `wasm-release`
- Card export binary: `crates/engine/src/bin/card_data_export.rs` (takes path or `FORGE_CARDS_PATH` env var)
- No existing README.md in repo
- Forge card files source: Card-Forge/forge GitHub repo, `forge-gui/res/cardsfolder/`

</specifics>
