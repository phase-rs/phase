<p align="center">
  <img src="client/public/logo.webp" alt="phase.rs" width="280" />
</p>

<p align="center">
  <strong>An open-source Magic: The Gathering rules engine and game client</strong>
</p>

<p align="center">
  <a href="https://preview.phase-rs.dev"><img alt="Play Preview" src="https://img.shields.io/badge/⚡_Play_Preview-Latest-f59e0b?style=for-the-badge"></a>
  <a href="https://phase-rs.dev"><img alt="Play Online" src="https://img.shields.io/badge/▶_Play_Online-Alpha-6366f1?style=for-the-badge"></a>
  <a href="https://github.com/phase-rs/phase/releases/latest"><img alt="Download" src="https://img.shields.io/badge/⬇_Download-Desktop-10b981?style=for-the-badge"></a>
</p>

<p align="center">
  <a href="#quick-start">Quick Start</a> · <a href="#features">Features</a> · <a href="#architecture">Architecture</a> · <a href="#development">Development</a> · <a href="https://discord.gg/dUZwhYHUyk">Discord</a> · <a href="https://deepwiki.com/phase-rs/phase"><img alt="Ask DeepWiki" src="https://deepwiki.com/badge.svg"></a>
</p>

<!-- coverage-badges:start -->
<p align="center">
  <img alt="Card Coverage" src="https://img.shields.io/endpoint?url=https%3A%2F%2Fpub-fc5b5c2c6e774356ae3e730bb0326394.r2.dev%2Fbadges%2Fcoverage.json">
  <img alt="Keywords" src="https://img.shields.io/endpoint?url=https%3A%2F%2Fpub-fc5b5c2c6e774356ae3e730bb0326394.r2.dev%2Fbadges%2Fkeywords.json">
  <img alt="Cards" src="https://img.shields.io/endpoint?url=https%3A%2F%2Fpub-fc5b5c2c6e774356ae3e730bb0326394.r2.dev%2Fbadges%2Fcards.json">
  <br/>
  <img alt="Pauper" src="https://img.shields.io/endpoint?url=https%3A%2F%2Fpub-fc5b5c2c6e774356ae3e730bb0326394.r2.dev%2Fbadges%2Fformat-pauper.json">
  <img alt="Pioneer" src="https://img.shields.io/endpoint?url=https%3A%2F%2Fpub-fc5b5c2c6e774356ae3e730bb0326394.r2.dev%2Fbadges%2Fformat-pioneer.json">
  <img alt="Modern" src="https://img.shields.io/endpoint?url=https%3A%2F%2Fpub-fc5b5c2c6e774356ae3e730bb0326394.r2.dev%2Fbadges%2Fformat-modern.json">
  <img alt="Standard" src="https://img.shields.io/endpoint?url=https%3A%2F%2Fpub-fc5b5c2c6e774356ae3e730bb0326394.r2.dev%2Fbadges%2Fformat-standard.json">
  <img alt="Legacy" src="https://img.shields.io/endpoint?url=https%3A%2F%2Fpub-fc5b5c2c6e774356ae3e730bb0326394.r2.dev%2Fbadges%2Fformat-legacy.json">
  <img alt="Vintage" src="https://img.shields.io/endpoint?url=https%3A%2F%2Fpub-fc5b5c2c6e774356ae3e730bb0326394.r2.dev%2Fbadges%2Fformat-vintage.json">
  <img alt="Commander" src="https://img.shields.io/endpoint?url=https%3A%2F%2Fpub-fc5b5c2c6e774356ae3e730bb0326394.r2.dev%2Fbadges%2Fformat-commander.json">
</p>
<!-- coverage-badges:end -->

---

<p align="center">
  <img src="docs/screenshot.webp" alt="phase.rs gameplay" width="900" />
</p>

A Rust-native MTG engine compiling to native and WASM, powering a Tauri desktop app, browser PWA, and WebSocket multiplayer. Implements comprehensive MTG rules using functional architecture — pure reducers, discriminated unions, and immutable state with structural sharing — with an Arena-quality React/TypeScript UI.

## Story

I'm Matt — a millennial software engineer who loves Magic. My six-year-old son asks me to play with him all the time, but the real game is just too complicated for a kid his age.

So I built [Alchemy](https://matthewevans.github.io/alchemy) ([source](https://github.com/matthewevans/alchemy)) — a simplified, kid-friendly version of MTG we could play together on our iPads. Fewer keywords, no lands, energy that builds each turn, and an adaptive learning mode where he solves math problems for combat bonuses. I had a working version in a single afternoon and fleshed it out over the following week.

After that I wanted to see how far I could push it — a real MTG rules engine in Rust, compiling to WASM, with the same React frontend so the whole thing runs in a browser. This whole project went from nothing to where it is now in a matter of weeks.

I'm not trying to make money off this. There are no ads. I'm just a dude who likes Magic.

## Features

- **Rules engine** — Turns, priority, stack, combat, state-based actions, layers, triggers, replacement effects
- **34,300+ cards** — Parsed from MTGJSON with format support (Commander, Modern, Pioneer, Standard, and more)
- **AI opponent** — Per-card decision logic, game tree search, and evaluation heuristics
- **Game UI** — Battlefield, hand, stack, targeting overlays, mana payment, animations, and ambient audio
- **Multiplayer** — WebSocket server with hidden information, lobby system, and WebRTC peer-to-peer
- **Metagame feeds** — Automated scraping of top decks from MTGGoldfish, updated daily
- **Deck builder** — Card search, visual builder, and `.dck`/`.dec` import
- **Cross-platform** — Tauri desktop (Windows, macOS, Linux), browser PWA, and tablet
- **Card images** — Scryfall integration with IndexedDB caching

## Quick Start

### Prerequisites

- [Rust toolchain](https://rustup.rs/)
- wasm32 target: `rustup target add wasm32-unknown-unknown`
- wasm-bindgen-cli: `cargo install wasm-bindgen-cli@0.2.114`
- wasm-opt (optional): `brew install binaryen` or `apt install binaryen`
- [Node.js](https://nodejs.org/) 22+ and [pnpm](https://pnpm.io/): `npm i -g pnpm`

### Setup

```bash
git clone https://github.com/phase-rs/phase && cd phase
./scripts/setup.sh     # Downloads card data, builds WASM, installs deps
cd client && pnpm dev  # Start dev server at localhost:5173
```

### Manual Steps

```bash
./scripts/gen-card-data.sh            # generate card-data.json
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
| `feed-scraper` | Metagame deck scraper (MTGGoldfish) |

Dependency flow: `engine` <- `phase-ai` <- `engine-wasm` / `server-core` <- `phase-server` (feed-scraper is standalone)

### Frontend (`client/`)

React + TypeScript + Tailwind v4 + Zustand + Framer Motion + Vite

Transport-agnostic `EngineAdapter` interface with multiple implementations:
- **WasmAdapter** — Direct WASM calls (browser/PWA)
- **TauriAdapter** — Tauri IPC (desktop)
- **WebSocketAdapter** — WebSocket (multiplayer)
- **P2PHostAdapter / P2PGuestAdapter** — WebRTC peer-to-peer via PeerJS

### Design Principles

- **Pure reducers** — `apply(state, action) -> ActionResult` with no mutation
- **Discriminated unions** — Rust enums serialize to tagged TS unions via serde + tsify
- **Structural sharing** — Immutable state via rpds persistent data structures

## Development

### Build Commands

```bash
# Rust (uses cargo-nextest for test execution)
cargo test-all                             # Run all tests (nextest)
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
cargo test-all          # Run all tests (nextest)
cargo clippy-strict     # Lint with -D warnings
cargo export-cards      # Run card data exporter
cargo coverage          # Card support coverage report
cargo wasm              # Build WASM (debug)
cargo wasm-release      # Build WASM (release)
cargo serve             # Run multiplayer server
cargo scrape-feeds      # Scrape metagame feeds
```

### Project Structure

```
crates/
  engine/             Core rules engine
  engine-wasm/        WASM bindings
  phase-ai/           AI opponent
  server-core/        Server session management
  phase-server/       Axum WebSocket server
  feed-scraper/       Metagame deck scraper
client/               React frontend
scripts/              Build and setup scripts
```

## Contact

- **Email**: [matt@phase-rs.dev](mailto:matt@phase-rs.dev)
- **Discord**: [discord.gg/dUZwhYHUyk](https://discord.gg/dUZwhYHUyk)

## Non-Commercial Fan Project

phase.rs is a non-commercial fan project built under the spirit of the
[Wizards of the Coast Fan Content Policy](https://company.wizards.com/en/legal/fancontentpolicy).
It exists for hobbyist, educational, and research use only.

- **No bundled Wizards assets.** This repository does not distribute MTG card
  images, card art, mana symbol artwork, card-frame graphics, or the
  Comprehensive Rules document. Card images and mana symbols are fetched from
  [Scryfall](https://scryfall.com/) at runtime by the user's browser. Card
  metadata is sourced from [MTGJSON](https://mtgjson.com/).
- **No affiliation.** phase.rs is not affiliated with, endorsed by, sponsored
  by, or approved by Wizards of the Coast LLC or Hasbro, Inc.

Magic: The Gathering, Planeswalker, the mana symbols, and all associated
names, text, and imagery are trademarks and copyrights of Wizards of the
Coast LLC. All rights reserved by their respective owners.

If you believe content in this repository infringes your rights, please see
[DMCA.md](DMCA.md).

## Acknowledgments

- [MTGJSON](https://mtgjson.com/) — Card data (MIT licensed)
- [Scryfall](https://scryfall.com/) — Card images and search API
- [MTGGoldfish](https://www.mtggoldfish.com/) — Metagame deck data
- [Wizards of the Coast](https://magic.wizards.com/) — Magic: The Gathering

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache 2.0](LICENSE-APACHE), at your option.
