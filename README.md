# phase.rs

An open-source Magic: The Gathering rules engine and game client.

The Rust engine compiles to both native and WASM, supporting a Tauri desktop app, browser PWA, and WebSocket multiplayer. It implements MTG comprehensive rules using functional architecture — pure reducers, discriminated unions, and immutable state with structural sharing — with an Arena-quality React/TypeScript UI.

## Features

- Full MTG rules engine (turns, priority, stack, combat, state-based actions)
- 32,300+ card definitions parsed from Forge's .txt format
- AI opponent with per-card decision logic and game tree search
- React game UI with battlefield, hand, stack, targeting, mana payment
- WebSocket multiplayer with hidden information
- Deck builder with card search and .dck/.dec import
- Desktop app via Tauri (Windows, macOS, Linux)
- PWA/WASM for browser and tablet
- Card images from Scryfall with IndexedDB caching

## Quick Start

### Prerequisites

- [Rust toolchain](https://rustup.rs/)
- wasm32 target: `rustup target add wasm32-unknown-unknown`
- wasm-bindgen-cli: `cargo install wasm-bindgen-cli@0.2.114`
- wasm-opt (optional): `brew install binaryen` or `apt install binaryen`
- [Node.js](https://nodejs.org/) 18+ and [pnpm](https://pnpm.io/): `npm i -g pnpm`

### Setup

```bash
git clone https://github.com/phase-rs/phase && cd phase
./scripts/setup.sh     # Downloads card data, builds WASM, installs deps
cd client && pnpm dev  # Start dev server at localhost:5173
```

### Manual Steps

```bash
./scripts/gen-card-data.sh            # Download Forge cards + generate card-data.json
./scripts/build-wasm.sh               # Build WASM bindings
cd client && pnpm install && pnpm dev # Start frontend
```

## Architecture

### Rust Workspace (`crates/`)

| Crate | Description |
|-------|-------------|
| `engine` | Core rules engine: types, game logic, parser, card database |
| `phase-ai` | AI opponent: evaluation, legal actions, search |
| `engine-wasm` | WASM bindings via wasm-bindgen + tsify |
| `server-core` | Server-side game session management |
| `phase-server` | Axum WebSocket server for multiplayer |

Dependency flow: `engine` <- `phase-ai` <- `engine-wasm` / `server-core` <- `phase-server`

### Frontend (`client/`)

React + TypeScript + Tailwind v4 + Zustand + Framer Motion + Vite

Transport-agnostic `EngineAdapter` interface with three implementations:
- **WasmAdapter** -- Direct WASM calls (browser/PWA)
- **TauriAdapter** -- Tauri IPC (desktop)
- **WsAdapter** -- WebSocket (multiplayer)

### Key Design Decisions

- Pure `apply(state, action) -> ActionResult` reducer pattern (no mutation)
- Discriminated unions across WASM boundary via serde + tsify
- Immutable state with structural sharing (rpds)
- Forge's .txt card format as upstream compatibility surface

## Development

### Build Commands

```bash
# Rust
cargo test --all                           # Run all tests
cargo clippy --all-targets -- -D warnings  # Lint
cargo fmt --all -- --check                 # Format check

# WASM
./scripts/build-wasm.sh                    # Build WASM (release)
./scripts/build-wasm.sh debug              # Build WASM (debug)

# Frontend
cd client
pnpm install                               # Install dependencies
pnpm dev                                   # Vite dev server
pnpm build                                 # TypeScript check + Vite build
pnpm lint                                  # ESLint
pnpm test                                  # Vitest
```

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

```
crates/
  engine/             Core rules engine
  engine-wasm/        WASM bindings
  phase-ai/           AI opponent
  server-core/        Server session management
  phase-server/       Axum WebSocket server
client/               React frontend
scripts/              Build and setup scripts
data/                 Downloaded Forge card files (gitignored)
.planning/            Project planning docs
```

## License

TBD
