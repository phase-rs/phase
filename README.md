<p align="center">
  <img src="client/public/logo.webp" alt="phase.rs" width="280" />
</p>

<p align="center">
  <strong>An open-source Magic: The Gathering rules engine and game client</strong>
</p>

<p align="center">
  <a href="#quick-start">Quick Start</a> · <a href="#features">Features</a> · <a href="#architecture">Architecture</a> · <a href="#development">Development</a>
</p>

---

A Rust-native MTG engine compiling to native and WASM, powering a Tauri desktop app, browser PWA, and WebSocket multiplayer. Implements comprehensive MTG rules using functional architecture — pure reducers, discriminated unions, and immutable state with structural sharing — with an Arena-quality React/TypeScript UI.

## Features

- **Rules engine** — Turns, priority, stack, combat, state-based actions, layers, triggers, replacement effects
- **32,300+ cards** — Parsed from Forge's card definitions with typed ability JSON
- **AI opponent** — Per-card decision logic, game tree search, and evaluation heuristics
- **Game UI** — Battlefield, hand, stack, targeting overlays, mana payment, and animations
- **Multiplayer** — WebSocket server with hidden information and lobby system
- **Deck builder** — Card search, visual builder, and `.dck`/`.dec` import
- **Cross-platform** — Tauri desktop (Windows, macOS, Linux), browser PWA, and tablet
- **Card images** — Scryfall integration with IndexedDB caching

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

### Design Principles

- **Pure reducers** — `apply(state, action) -> ActionResult` with no mutation
- **Discriminated unions** — Rust enums serialize to tagged TS unions via serde + tsify
- **Structural sharing** — Immutable state via rpds persistent data structures
- **Forge compatibility** — Card definitions parsed from Forge's upstream format

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

Dual-licensed under [MIT](LICENSE-MIT) or [Apache 2.0](LICENSE-APACHE), at your option.
