---
phase: quick-1
plan: 01
type: execute
wave: 1
depends_on: []
files_modified:
  - scripts/gen-card-data.sh
  - .cargo/config.toml
  - README.md
autonomous: true
requirements: [QUICK-1]
must_haves:
  truths:
    - "Running `cargo gen-cards` downloads Forge card files and produces client/public/card-data.json"
    - "Running `cargo setup` gets a fresh clone from zero to runnable"
    - "README explains what the project is, how to set up, build, and play"
  artifacts:
    - path: "scripts/gen-card-data.sh"
      provides: "Downloads Forge cardsfolder, runs card_data_export, outputs card-data.json"
    - path: ".cargo/config.toml"
      provides: "Cargo aliases for gen-cards, setup, serve, dev"
    - path: "README.md"
      provides: "User-facing showcase + developer documentation"
  key_links:
    - from: ".cargo/config.toml"
      to: "scripts/gen-card-data.sh"
      via: "cargo gen-cards alias"
      pattern: "gen-cards"
    - from: "scripts/gen-card-data.sh"
      to: "crates/engine/src/bin/card_data_export.rs"
      via: "cargo run --bin card-data-export"
      pattern: "card.data.export"
---

<objective>
Add card-data generation pipeline, expand cargo aliases as a task runner, and create a comprehensive README.

Purpose: Enable first-run setup from git clone to running app, and provide project documentation.
Output: gen-card-data.sh script, expanded .cargo/config.toml, README.md
</objective>

<execution_context>
@/Users/matt/.claude/get-shit-done/workflows/execute-plan.md
@/Users/matt/.claude/get-shit-done/templates/summary.md
</execution_context>

<context>
@.planning/PROJECT.md
@.cargo/config.toml
@scripts/build-wasm.sh
@crates/engine/src/bin/card_data_export.rs
@CLAUDE.md
</context>

<tasks>

<task type="auto">
  <name>Task 1: Create card-data generation script and expand cargo aliases</name>
  <files>scripts/gen-card-data.sh, .cargo/config.toml</files>
  <action>
Create `scripts/gen-card-data.sh` (bash, executable):

1. Set `set -euo pipefail`
2. Define variables:
   - `FORGE_REPO_URL="https://github.com/Card-Forge/forge"`
   - `CARDS_DIR="data/cardsfolder"` (local cache of downloaded cards)
   - `OUTPUT="client/public/card-data.json"`
3. Download Forge cardsfolder:
   - If `data/cardsfolder/` already exists, skip download (print "Card files already present, skipping download. Delete data/cardsfolder/ to re-download.")
   - Otherwise: use `git clone --depth 1 --filter=blob:none --sparse` the Forge repo into a temp dir, then `git sparse-checkout set forge-gui/res/cardsfolder`, then copy `forge-gui/res/cardsfolder/` to `data/cardsfolder/`, then remove the temp dir
   - This avoids cloning the entire 2GB+ Forge repo
4. Build and run the card_data_export binary:
   - `cargo run --release --bin card-data-export -- "$CARDS_DIR" > "$OUTPUT"`
5. Print summary: file size and card count (use `wc -c` and a quick `grep -c` or jq count)

Add `data/` to `.gitignore` (the downloaded Forge card files should not be committed).

Expand `.cargo/config.toml` with aliases. Keep existing `wasm` and `wasm-release`. Add:
- `gen-cards = "xtask scripts/gen-card-data.sh"` — No, cargo aliases only support cargo subcommands. Instead, document these as shell commands.

Actually, cargo aliases can only alias cargo commands (they prepend `cargo` to the value). So for shell scripts, we need a different approach. Update `.cargo/config.toml` with these cargo-native aliases:

```toml
[alias]
wasm = "build --package engine-wasm --target wasm32-unknown-unknown"
wasm-release = "build --package engine-wasm --target wasm32-unknown-unknown --release"
test-all = "test --all"
clippy-strict = "clippy --all-targets -- -D warnings"
export-cards = "run --release --bin card-data-export --"
serve = "run --release --bin forge-server"
```

The `gen-cards` workflow (download + export) stays as the shell script since it involves git operations. The `export-cards` alias covers the cargo-native part. Create a `scripts/setup.sh` script that orchestrates full first-run:

```bash
#!/usr/bin/env bash
set -euo pipefail
echo "=== Forge.rs Setup ==="
echo "Step 1/4: Generating card data..."
./scripts/gen-card-data.sh
echo "Step 2/4: Building WASM..."
./scripts/build-wasm.sh
echo "Step 3/4: Installing frontend dependencies..."
(cd client && pnpm install)
echo "Step 4/4: Done!"
echo ""
echo "Run 'cd client && pnpm dev' to start the dev server."
```

Make both scripts executable (`chmod +x`).
  </action>
  <verify>
    <automated>bash -c "test -x scripts/gen-card-data.sh && test -x scripts/setup.sh && grep -q 'export-cards' .cargo/config.toml && grep -q 'serve' .cargo/config.toml && echo 'PASS'"</automated>
  </verify>
  <done>gen-card-data.sh downloads Forge cards via sparse checkout and runs card_data_export. setup.sh orchestrates full first-run. Cargo aliases expanded with test-all, clippy-strict, export-cards, serve.</done>
</task>

<task type="auto">
  <name>Task 2: Create README.md</name>
  <files>README.md</files>
  <action>
Create `README.md` with user-facing showcase first, developer docs below. Use content from PROJECT.md and CLAUDE.md for accuracy. Structure:

```
# Forge.rs

[One-line description: Magic: The Gathering game engine in Rust + React]

[2-3 sentence elevator pitch from PROJECT.md core value. Mention: Rust engine compiling to native + WASM, React frontend, AI opponent, 32k+ Forge card definitions, multiplayer]

## Features

- Full MTG rules engine (turns, priority, stack, combat, state-based actions)
- 32,300+ card definitions from Forge's .txt format
- AI opponent with per-card decision logic and game tree search
- React game UI with battlefield, hand, stack, targeting, mana payment
- WebSocket multiplayer with hidden information
- Deck builder with card search and .dck/.dec import
- Desktop app via Tauri (Windows, macOS, Linux)
- PWA/WASM for browser and tablet
- Card images from Scryfall with IndexedDB caching

## Screenshots

[placeholder: _Coming soon_]

## Quick Start

### Prerequisites

- Rust toolchain (rustup.rs)
- wasm-bindgen-cli v0.2.114 (`cargo install wasm-bindgen-cli@0.2.114`)
- wasm-opt (optional, `brew install binaryen` / apt)
- Node.js 18+ and pnpm (`npm i -g pnpm`)
- wasm32-unknown-unknown target (`rustup target add wasm32-unknown-unknown`)

### Setup

```bash
git clone https://github.com/[user]/forge.rs && cd forge.rs
./scripts/setup.sh    # Downloads card data, builds WASM, installs deps
cd client && pnpm dev  # Start dev server at localhost:5173
```

### Manual Steps

```bash
./scripts/gen-card-data.sh           # Download Forge cards + generate card-data.json
./scripts/build-wasm.sh              # Build WASM bindings
cd client && pnpm install && pnpm dev
```

## Architecture

### Rust Workspace

[Table or list of crates with one-line descriptions, from CLAUDE.md]
- `engine` — Core rules engine: types, game logic, parser, card database
- `forge-ai` — AI opponent: evaluation, legal actions, search
- `engine-wasm` — WASM bindings via wasm-bindgen + tsify
- `server-core` — Server-side game session management
- `forge-server` — Axum WebSocket server for multiplayer

Dependency flow: `engine` <- `forge-ai` <- `engine-wasm` / `server-core` <- `forge-server`

### Frontend (client/)

React + TypeScript + Tailwind v4 + Zustand + Framer Motion + Vite

Transport-agnostic EngineAdapter: WasmAdapter (browser), TauriAdapter (desktop), WsAdapter (multiplayer)

### Key Design Decisions

- Pure `apply(state, action) -> ActionResult` reducer pattern (no mutation)
- Discriminated unions across WASM boundary via serde + tsify
- Immutable state with structural sharing (rpds)
- Forge's .txt card format as upstream compatibility surface

## Development

### Build Commands

[From CLAUDE.md: cargo test, clippy, fmt, wasm build, client commands]

### Cargo Aliases

```
cargo test-all          # Run all tests
cargo clippy-strict     # Lint with -D warnings
cargo export-cards      # Run card data exporter
cargo wasm              # Build WASM (debug)
cargo wasm-release      # Build WASM (release)
cargo serve             # Run multiplayer server
```

### Project Structure

Brief tree showing top-level layout (crates/, client/, scripts/, data/).

## License

[MIT or whatever is in the repo — check for LICENSE file, if none say "TBD"]
```

Keep it concise. No filler. Real information only. Check for an existing LICENSE file to reference.
  </action>
  <verify>
    <automated>bash -c "test -f README.md && head -1 README.md | grep -q 'Forge' && grep -q 'Quick Start' README.md && grep -q 'Architecture' README.md && echo 'PASS'"</automated>
  </verify>
  <done>README.md exists with user-facing showcase (features, quick start) and developer docs (architecture, build commands, cargo aliases, project structure).</done>
</task>

</tasks>

<verification>
- `scripts/gen-card-data.sh` is executable and contains sparse checkout logic
- `scripts/setup.sh` is executable and orchestrates full setup
- `.cargo/config.toml` has all aliases (wasm, wasm-release, test-all, clippy-strict, export-cards, serve)
- `README.md` has both user-facing and developer sections
- `.gitignore` includes `data/` directory
</verification>

<success_criteria>
A new contributor can clone the repo, read the README, run `./scripts/setup.sh`, and have a working dev environment. Cargo aliases provide convenient shortcuts for common tasks.
</success_criteria>

<output>
After completion, create `.planning/quick/1-add-card-data-generation-cargo-script-al/1-SUMMARY.md`
</output>
