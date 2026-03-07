# Technology Stack

**Project:** Forge.ts -- MTG Rules Engine & Game Client
**Researched:** 2026-03-07

## Recommended Stack

### Core Engine (Rust)

| Technology | Version | Purpose | Why | Confidence |
|------------|---------|---------|-----|------------|
| Rust (stable) | 1.85+ | Engine language | Compiles to both native (Tauri) and wasm32-unknown-unknown from single codebase. Ownership model prevents game state bugs. Pattern matching is ideal for MTG's 202 effect types. | HIGH |
| serde + serde_json | 1.x | Serialization | Universal Rust serialization. Derive macros generate (de)serializers for game state, card definitions, IPC payloads. Required by Tauri commands. | HIGH |
| serde-wasm-bindgen | 0.6.x | WASM serialization | Direct Rust-to-JS value conversion without JSON intermediary. Smaller code size and faster than JSON round-tripping for WASM builds. Officially preferred approach. | HIGH |
| rpds | 1.85.x | Persistent data structures | Structural sharing for game state (critical for AI tree search -- cloning full MTG state is 10-50x larger than simple card games). Actively maintained (last release ~4 months ago). Thread-safe with Arc, or faster single-threaded with Rc. | MEDIUM |
| wasm-bindgen | 0.2.114 | WASM-JS bridge | Generates JS bindings for exported Rust functions. Actively maintained under new wasm-bindgen org after rustwasm sunset. Required for PWA/WASM target. | HIGH |
| tsify-next | 0.5.4 | TS type generation | Auto-generates TypeScript type definitions from Rust structs/enums via derive macros. Keeps frontend types in sync with engine types without manual maintenance. | MEDIUM |
| proptest | 1.10.0 | Property-based testing | Hypothesis-like testing for rules engine. Auto-generates game states, validates invariants (e.g., "state-based actions never leave a creature with 0 toughness on battlefield"). Shrinking finds minimal failing cases. | HIGH |

### Why NOT ECS

ECS (Bevy ECS, hecs, specs) is the dominant Rust game engine pattern, but it is **wrong for this project**:

- **MTG is not an ECS problem.** ECS excels at thousands of homogeneous entities updated per frame (physics, rendering). MTG has ~50-200 heterogeneous permanents with complex, rule-defined interactions evaluated on discrete events, not per-frame ticks.
- **Game tree search requires cheap state cloning.** AI needs to clone-and-explore thousands of game states. ECS world cloning is expensive and not designed for this. Persistent data structures with structural sharing are purpose-built for it.
- **The port plan's functional architecture is correct.** Discriminated unions (Rust enums) + pure reducer functions + immutable state maps directly to MTG's rules structure. Effect handlers keyed by ApiType string are the right pattern.

### Why NOT `im` crate

The `im` crate (v15.1.0) provides similar persistent data structures but has **not been updated since April 2022**. It is effectively unmaintained. `rpds` (v1.85.0, updated recently) provides the same structural sharing with active maintenance, `no_std` support, and configurable thread safety (Rc vs Arc).

### Desktop Shell (Tauri)

| Technology | Version | Purpose | Why | Confidence |
|------------|---------|---------|-----|------------|
| Tauri | 2.10.x | Desktop wrapper | Native performance, small binary (~5-10MB vs Electron's 100MB+). v2 rewrote IPC layer for fast binary transfer. iOS/Android support for future. | HIGH |
| tauri::ipc::Channel | (built-in) | Streaming IPC | For pushing game state updates from Rust to React without polling. Ordered message delivery. Used for game events, state diffs, AI thinking updates. | HIGH |
| tauri::ipc::Response | (built-in) | Binary IPC | Raw byte transfer for large payloads (card database loading, image cache). Avoids JSON serialization overhead. | MEDIUM |

#### Tauri v2 IPC Architecture

Use **Commands** for request/response (cast spell, get legal actions, load deck) and **Events** for fire-and-forget notifications (game state changed, AI thinking, animation triggers). The v2 IPC uses custom protocols (not string serialization like v1), so performance is adequate for real-time game state updates.

**Pattern for game state sync:**

```rust
// Rust side: Command returns current state, Channel pushes updates
#[tauri::command]
fn start_game(config: GameConfig, channel: Channel<GameStateUpdate>) -> Result<GameState, String> {
    let state = engine::new_game(config);
    // Channel pushes state diffs as game progresses
    Ok(state)
}

#[tauri::command]
fn submit_action(action: GameAction, app: AppHandle) -> Result<GameState, String> {
    // Process action, return new state, emit events for triggers/animations
    let (new_state, events) = engine::process_action(action);
    app.emit("game-events", &events)?;
    Ok(new_state)
}
```

### Frontend (React + TypeScript)

| Technology | Version | Purpose | Why | Confidence |
|------------|---------|---------|-----|------------|
| React | 19.x | UI framework | Component model is natural for card game UI (cards, zones, stack). Large ecosystem. Team familiarity from Alchemy project. | HIGH |
| TypeScript | 5.7+ | Type safety | Discriminated union actions from Rust map directly to TS types. tsify-next generates matching types. | HIGH |
| Vite | 6.x | Build tool | Fast HMR, Tauri's official recommended bundler. Handles both dev server (Tauri webview) and production build (PWA). | HIGH |
| pnpm | 9.x | Package manager | Fast, disk-efficient. Strict dependency resolution prevents phantom deps. | HIGH |
| Zustand | 5.0.x | State management | Minimal API, works outside React (useful for WASM adapter layer). No provider boilerplate. Uses native useSyncExternalStore in v5. Proven pattern from Alchemy. | HIGH |
| Tailwind CSS | 4.x | Styling | Utility-first is efficient for game UI layouts (grid-based battlefield, card overlays, responsive zones). Zero-runtime CSS. | MEDIUM |
| Framer Motion | 12.x | Animation | Card movement between zones, combat arrows, damage numbers, stack resolution. Declarative animation model works with React's render cycle. | MEDIUM |

#### State Management Pattern: Zustand + Adapter Layer

The key architectural decision is a **thin adapter layer** between the engine and Zustand store. This adapter swaps between Tauri IPC (native) and direct WASM calls (PWA) without the React components knowing the difference:

```typescript
// adapter.ts -- swaps between Tauri IPC and WASM
interface EngineAdapter {
  startGame(config: GameConfig): Promise<GameState>;
  submitAction(action: GameAction): Promise<GameState>;
  getLegalActions(playerId: string): Promise<GameAction[]>;
  onGameEvent(handler: (event: GameEvent) => void): void;
}

// Tauri adapter: uses @tauri-apps/api invoke + listen
const tauriAdapter: EngineAdapter = { /* invoke('start_game', ...) */ };

// WASM adapter: calls WASM module directly
const wasmAdapter: EngineAdapter = { /* wasm.start_game(...) */ };

// Zustand store consumes adapter
const useGameStore = create<GameStore>((set) => ({
  state: null,
  adapter: detectPlatform() === 'tauri' ? tauriAdapter : wasmAdapter,
  submitAction: async (action) => {
    const newState = await get().adapter.submitAction(action);
    set({ state: newState });
  },
}));
```

### WASM Tooling

| Technology | Version | Purpose | Why | Confidence |
|------------|---------|---------|-----|------------|
| wasm-bindgen-cli | 0.2.114 | WASM output processing | Generates JS glue code from cargo build output. Direct replacement for archived wasm-pack. Pin version to match wasm-bindgen crate version. | HIGH |
| wasm-opt | latest | WASM optimization | Binaryen optimizer. Reduces WASM binary size 20-40%. Run in release builds only. | HIGH |
| cargo build --target wasm32-unknown-unknown | (built-in) | WASM compilation | Standard Rust WASM target. No additional toolchain needed beyond `rustup target add wasm32-unknown-unknown`. | HIGH |

#### Why NOT wasm-pack

wasm-pack was **archived in July 2025** when the rustwasm GitHub org was sunset. It is no longer maintained. The recommended replacement is using the individual tools directly:

```bash
# Build WASM
cargo build --target wasm32-unknown-unknown --release -p forge-engine

# Generate JS bindings (replaces wasm-pack)
wasm-bindgen --target web \
  ./target/wasm32-unknown-unknown/release/forge_engine.wasm \
  --out-dir ./src/wasm/

# Optimize (optional, release only)
wasm-opt -Oz ./src/wasm/forge_engine_bg.wasm -o ./src/wasm/forge_engine_bg.wasm
```

#### Why NOT Trunk

Trunk is a WASM web application bundler designed for pure-Rust frontends (Yew, Leptos). This project uses React for the frontend, so Trunk adds no value. Vite handles the frontend build; wasm-bindgen-cli handles the WASM output.

### Testing

| Technology | Version | Purpose | Why | Confidence |
|------------|---------|---------|-----|------------|
| `cargo test` | (built-in) | Rust unit/integration tests | Standard Rust testing. Module-level unit tests for effect handlers, integration tests for full game sequences. | HIGH |
| proptest | 1.10.0 | Property-based testing (Rust) | Generate random game states, validate invariants. Critical for rules engine correctness across 202 effect types and 137 trigger interactions. | HIGH |
| Vitest | 4.x | Frontend unit tests | 2-10x faster than Jest. Native Vite integration. Use with React Testing Library for component tests. | HIGH |
| React Testing Library | 16.x | Component testing | Test user behavior (click card, select target) not implementation details. | HIGH |
| Playwright | 1.50+ | E2E testing | Cross-browser support (Chrome, Firefox, WebKit). Can test Tauri desktop apps via WebDriver. Better architecture for desktop app testing than Cypress (runs externally, not inside browser). | HIGH |

#### Why NOT Cypress for E2E

Cypress runs inside the browser's execution loop, which creates friction when testing Tauri desktop apps. Playwright runs externally via CDP, making it better suited for testing apps in a webview context. Playwright also supports parallel execution natively and has broader browser coverage (including WebKit/Safari).

### Build & Project Structure

| Technology | Version | Purpose | Why | Confidence |
|------------|---------|---------|-----|------------|
| Cargo workspace | (built-in) | Rust monorepo | Multiple crates: engine-core, card-parser, tauri-app, wasm-bridge. Shared dependencies, independent compilation targets. | HIGH |
| Vite | 6.x | Frontend build | Dev server with HMR for Tauri webview. Production build for PWA. Handles WASM module loading. | HIGH |
| pnpm workspace | 9.x | JS monorepo (if needed) | Only if frontend grows to multiple packages. Start with single package. | LOW |

#### Cargo Workspace Layout

```
forge.ts/
  Cargo.toml              # Workspace root
  crates/
    forge-core/           # Pure game engine (no platform deps)
      Cargo.toml          # Targets: lib (native + wasm)
      src/
        lib.rs
        state/            # GameState, zones, players
        effects/          # 202 effect handlers
        triggers/         # 137 trigger matchers
        statics/          # Layer system (Rule 613)
        combat/           # Combat resolution
        parser/           # Card .txt file parser
        ai/               # AI decision engine
    forge-tauri/          # Tauri-specific: commands, state management
      Cargo.toml          # Depends on forge-core, tauri
      src/
        main.rs
        commands.rs       # #[tauri::command] functions
    forge-wasm/           # WASM bindings: exports for JS
      Cargo.toml          # Depends on forge-core, wasm-bindgen
      src/
        lib.rs            # #[wasm_bindgen] exports
  src/                    # React frontend
    adapter/              # Engine adapter (Tauri IPC vs WASM)
    components/           # React game UI
    store/                # Zustand stores
    types/                # Generated from tsify-next
  tauri.conf.json
  vite.config.ts
  package.json
```

**Key insight:** `forge-core` has zero platform dependencies. It compiles to both native (used by `forge-tauri` via direct Rust calls) and WASM (used by `forge-wasm` via wasm-bindgen exports). The React frontend talks to either through the adapter layer.

### Infrastructure & Distribution

| Technology | Purpose | Why | Confidence |
|------------|---------|-----|------------|
| Scryfall API | Card images | Free for non-commercial. Comprehensive. On-demand loading with local disk cache. | HIGH |
| Service Worker (Workbox) | PWA offline | Cache WASM module + static assets. Card images cached on first load. | MEDIUM |
| GitHub Releases | Distribution | Tauri bundles (.dmg, .msi, .AppImage) via `tauri build`. No app store needed. | HIGH |
| GitHub Actions | CI/CD | Build + test on push. Cross-platform Tauri builds. WASM build + deploy to GitHub Pages for PWA. | HIGH |

## Alternatives Considered

| Category | Recommended | Alternative | Why Not |
|----------|-------------|-------------|---------|
| Persistent data structures | rpds | im | im unmaintained since April 2022. rpds actively maintained, similar API. |
| WASM tooling | wasm-bindgen-cli + wasm-opt | wasm-pack | wasm-pack archived July 2025. Dead project. |
| WASM tooling | wasm-bindgen-cli | Trunk | Trunk is for pure-Rust frontends (Yew/Leptos). We use React. |
| State management | Zustand | Redux Toolkit | RTK is heavier, more boilerplate. Zustand's minimal API suits adapter pattern better. |
| State management | Zustand | Jotai | Jotai is bottom-up atomic state. Game state is a single large tree pushed from engine -- top-down store (Zustand) fits better. |
| Game architecture | Functional reducers + rpds | ECS (Bevy/hecs) | MTG is discrete event-driven with complex rule interactions, not per-frame entity processing. AI tree search needs cheap state cloning. |
| E2E testing | Playwright | Cypress | Playwright's external architecture works better with Tauri desktop apps. Broader browser support. |
| Serialization (WASM) | serde-wasm-bindgen | serde_json round-trip | Direct JS value conversion avoids JSON intermediary. Smaller code, faster transfer. |
| Animation | Framer Motion | React Spring | Framer Motion has better layout animation support for card repositioning between zones. |
| CSS | Tailwind | CSS Modules | Utility-first is faster for iterating on game layout. No runtime cost. |

## Installation

### Rust

```bash
# Install Rust toolchain
rustup default stable
rustup target add wasm32-unknown-unknown

# Install WASM tools
cargo install wasm-bindgen-cli@0.2.114
cargo install wasm-opt  # or install binaryen via package manager
```

### Cargo.toml (workspace root)

```toml
[workspace]
members = ["crates/*"]
resolver = "2"

[workspace.dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
proptest = "1.10"
```

### Cargo.toml (forge-core)

```toml
[package]
name = "forge-core"
version = "0.1.0"
edition = "2024"

[dependencies]
serde = { workspace = true }
serde_json = { workspace = true }
rpds = "1"

[dev-dependencies]
proptest = { workspace = true }
```

### Cargo.toml (forge-wasm)

```toml
[package]
name = "forge-wasm"
version = "0.1.0"
edition = "2024"

[lib]
crate-type = ["cdylib"]

[dependencies]
forge-core = { path = "../forge-core" }
wasm-bindgen = "0.2.114"
serde-wasm-bindgen = "0.6"
tsify-next = { version = "0.5", features = ["js"] }
serde = { workspace = true }
```

### Cargo.toml (forge-tauri)

```toml
[package]
name = "forge-tauri"
version = "0.1.0"
edition = "2024"

[dependencies]
forge-core = { path = "../forge-core" }
tauri = { version = "2", features = [] }
tauri-build = { version = "2", features = [] }
serde = { workspace = true }
serde_json = { workspace = true }

[build-dependencies]
tauri-build = { version = "2", features = [] }
```

### Frontend (package.json)

```bash
pnpm add react react-dom zustand framer-motion
pnpm add -D typescript @types/react @types/react-dom \
  vite @vitejs/plugin-react \
  tailwindcss \
  vitest @testing-library/react @testing-library/jest-dom \
  playwright @playwright/test \
  @tauri-apps/api @tauri-apps/cli
```

## Version Pinning Strategy

- **wasm-bindgen** and **wasm-bindgen-cli** MUST be the same version. Pin both to 0.2.114. Mismatched versions cause cryptic build failures.
- **Tauri** crate and **@tauri-apps/api** npm package should use compatible major versions (both v2).
- **tsify-next** version must be compatible with your wasm-bindgen version. Test after any wasm-bindgen upgrade.

## Sources

- [Tauri v2 IPC Concepts](https://v2.tauri.app/concept/inter-process-communication/)
- [Tauri v2 Calling Rust from Frontend](https://v2.tauri.app/develop/calling-rust/)
- [Tauri v2 Calling Frontend from Rust](https://v2.tauri.app/develop/calling-frontend/)
- [Tauri v2 Stable Release Blog](https://v2.tauri.app/blog/tauri-20/)
- [wasm-bindgen Guide](https://rustwasm.github.io/docs/wasm-bindgen/)
- [wasm-bindgen GitHub (new org)](https://github.com/wasm-bindgen/wasm-bindgen)
- [Life after wasm-pack](https://nickb.dev/blog/life-after-wasm-pack-an-opinionated-deconstruction/)
- [Sunsetting rustwasm GitHub Org](https://blog.rust-lang.org/inside-rust/2025/07/21/sunsetting-the-rustwasm-github-org/)
- [serde-wasm-bindgen docs](https://docs.rs/serde-wasm-bindgen/latest/serde_wasm_bindgen/)
- [rpds GitHub](https://github.com/orium/rpds)
- [im-rs GitHub (unmaintained)](https://github.com/bodil/im-rs)
- [proptest GitHub](https://github.com/proptest-rs/proptest)
- [tsify-next GitHub](https://github.com/AmbientRun/tsify-next)
- [Zustand v5 Announcement](https://pmnd.rs/blog/announcing-zustand-v5)
- [Vitest](https://vitest.dev/)
- [Tauri Project Structure](https://v2.tauri.app/start/project-structure/)
- [Tauri State Management](https://v2.tauri.app/develop/state-management/)
