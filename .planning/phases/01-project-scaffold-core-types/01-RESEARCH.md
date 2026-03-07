# Phase 1: Project Scaffold & Core Types - Research

**Researched:** 2026-03-07
**Domain:** Rust/WASM build toolchain, React+Vite frontend, Rust-to-TypeScript type generation, CI/CD
**Confidence:** HIGH

## Summary

Phase 1 establishes the entire build infrastructure: a Cargo workspace that compiles Rust to both native and WASM targets, a React frontend that imports and calls WASM bindings, auto-generated TypeScript types from Rust via `tsify`, and a CI pipeline. The Rust-to-WASM toolchain has matured significantly -- `wasm-bindgen` + `wasm-bindgen-cli` is the standard path (wasm-pack is unmaintained), and `tsify` (the original, not `tsify-next`) generates TypeScript discriminated unions from Rust enums via serde attributes. The build pipeline is a three-step process: `cargo build --target wasm32-unknown-unknown`, then `wasm-bindgen --target web`, then `wasm-opt` for size optimization.

**Critical correction from CONTEXT.md:** The discussion mentions `tsify-next`, but as of August 2025 (RUSTSEC-2025-0048), `tsify-next` is officially unmaintained. All its features have been merged back into the original `tsify` crate (v0.5.5+). Use `tsify`, not `tsify-next`.

**Primary recommendation:** Use a Cargo workspace with separate `engine` (lib) and `engine-wasm` (cdylib) crates, `tsify` v0.5.5 with the `js` feature for type generation, Vite 7 + `vite-plugin-wasm` for the React frontend, and GitHub Actions with `actions-rust-lang/setup-rust-toolchain` for CI.

<user_constraints>

## User Constraints (from CONTEXT.md)

### Locked Decisions
- wasm-pack is dead/unmaintained -- use `wasm-bindgen` + `wasm-bindgen-cli` directly
- Vite imports WASM via `vite-plugin-wasm` -- no intermediate tooling
- All Rust-to-TypeScript type generation via `tsify-next` -- single source of truth, zero manual TS types for shared engine types (NOTE: research finds `tsify-next` is unmaintained per RUSTSEC-2025-0048; use `tsify` instead -- same API, original maintainer)
- Manual TypeScript types only for frontend-only concerns (UI state, component props)
- Rich adapter interface -- async queuing, error normalization, command pattern
- React components call `adapter.submitAction(action)` without knowing the transport
- Phase 1 implements WasmAdapter only -- TauriAdapter deferred to Phase 7
- Interface designed to support both transports from day one
- Foundation types beyond success criteria: Player, CardId/ObjectId, CardDefinition (stub), Color, ManaPool, AbilityType (enum variants)
- GameAction and GameEvent are Rust enums (discriminated unions) from day one
- tsify generates TypeScript discriminated unions automatically
- Phase 1 defines top-level variants; later phases add data to each variant
- Exhaustive pattern matching over trait objects

### Claude's Discretion
- **Zone modeling**: Claude picks the approach that best fits MTG's zone rules (typed zone structs vs enum + map)
- **Forge awareness in Phase 1 types**: Claude decides whether CardDefinition stub mirrors Forge field names or stays abstract
- **Workspace layout**: Idiomatic Cargo workspace structure -- user wants best-practice clean architecture
- **CI pipeline**: GitHub Actions with best-practice checks (tests, clippy, formatting, WASM size reporting)
- **React app structure**: Standard Vite + React + TypeScript setup

### Deferred Ideas (OUT OF SCOPE)
None -- discussion stayed within phase scope

</user_constraints>

<phase_requirements>

## Phase Requirements

| ID | Description | Research Support |
|----|-------------|-----------------|
| PLAT-03 | EngineAdapter abstraction (Tauri IPC and WASM bindings) | Async trait pattern with WasmAdapter using wasm-bindgen + serde-wasm-bindgen for serialization; interface designed for both WASM and Tauri IPC transports |

</phase_requirements>

## Standard Stack

### Core

| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| wasm-bindgen | 0.2.114 | Rust-JS interop for WASM | De facto standard for Rust WASM, generates JS glue code and TS declarations |
| wasm-bindgen-cli | 0.2.114 | Post-build WASM processing | Must match wasm-bindgen version exactly; generates JS/TS bindings from compiled WASM |
| tsify | 0.5.5 | Rust types to TypeScript definitions | Generates TS types from Rust structs/enums via derive macro; merged tsify-next features |
| serde | 1.x | Serialization framework | Required by tsify and wasm-bindgen for data crossing the WASM boundary |
| serde-wasm-bindgen | 0.6.5 | Native serde-to-JsValue conversion | Faster than JSON for most cases, supports Map/Set/ArrayBuffer types that JSON cannot |
| rpds | 1.2.0 | Persistent data structures | Structural sharing for immutable game state; List, Vector, HashTrieMap, HashTrieSet |
| React | 19.x | UI framework | Standard React with TypeScript |
| Vite | 7.3.x | Build tool / dev server | Current stable, Rust-powered speed |
| vite-plugin-wasm | 3.5.0 | WASM ESM integration for Vite | Enables importing WASM modules as ES modules; supports Vite 2.x-7.x |
| vite-plugin-top-level-await | 1.6.0 | Top-level await support | Required by vite-plugin-wasm unless build.target is esnext |
| pnpm | 9.x | Package manager | Fast, disk-efficient, workspace support |
| TypeScript | 5.x | Type system | Latest stable |

### Supporting

| Library | Version | Purpose | When to Use |
|---------|---------|---------|-------------|
| wasm-opt (binaryen) | latest | WASM binary optimization | Post-build step; 10-20% size reduction over raw LLVM output |
| Vitest | 4.x | Frontend testing | Unit/integration tests for React components and TS adapter code |

### Alternatives Considered

| Instead of | Could Use | Tradeoff |
|------------|-----------|----------|
| tsify | ts-rs | ts-rs generates to files, not inline with wasm-bindgen; tsify integrates directly with WASM ABI |
| serde-wasm-bindgen | serde_json (via wasm-bindgen) | JSON is sometimes faster in browsers but cannot represent Map/Set/ArrayBuffer; serde-wasm-bindgen is officially preferred |
| vite-plugin-wasm | Vite ?init import | ?init requires manual async initialization; plugin provides seamless ESM imports |

**Installation (Rust):**
```toml
# Cargo.toml (workspace root)
[workspace]
resolver = "2"
members = ["crates/*"]

# crates/engine/Cargo.toml
[dependencies]
serde = { version = "1", features = ["derive"] }
rpds = { version = "1.2", features = ["serde"] }

# crates/engine-wasm/Cargo.toml
[lib]
crate-type = ["cdylib"]

[dependencies]
engine = { path = "../engine" }
wasm-bindgen = "0.2.114"
tsify = { version = "0.5", features = ["js"] }
serde = { version = "1", features = ["derive"] }
serde-wasm-bindgen = "0.6"
```

**Installation (Frontend):**
```bash
pnpm create vite client --template react-ts
cd client
pnpm add vite-plugin-wasm vite-plugin-top-level-await
pnpm add -D vitest
```

## Architecture Patterns

### Recommended Project Structure
```
forge-ts/
├── Cargo.toml                    # Workspace root
├── Cargo.lock
├── .cargo/
│   └── config.toml               # WASM target config
├── crates/
│   ├── engine/                   # Pure Rust game engine (lib)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── types/            # Core type definitions
│   │       │   ├── mod.rs
│   │       │   ├── game_state.rs
│   │       │   ├── actions.rs
│   │       │   ├── events.rs
│   │       │   ├── zones.rs
│   │       │   ├── mana.rs
│   │       │   ├── player.rs
│   │       │   ├── card.rs       # CardDefinition stub
│   │       │   └── identifiers.rs # CardId, ObjectId
│   │       └── phase.rs          # Turn phases
│   └── engine-wasm/              # WASM bindings (cdylib)
│       ├── Cargo.toml
│       └── src/
│           └── lib.rs            # #[wasm_bindgen] exports + tsify re-exports
├── client/                       # React frontend
│   ├── package.json
│   ├── vite.config.ts
│   ├── tsconfig.json
│   ├── src/
│   │   ├── main.tsx
│   │   ├── App.tsx
│   │   ├── adapter/             # EngineAdapter abstraction
│   │   │   ├── types.ts         # EngineAdapter interface
│   │   │   ├── wasm-adapter.ts  # WasmAdapter implementation
│   │   │   └── index.ts
│   │   └── wasm/                # Generated WASM output (gitignored)
│   │       ├── engine_wasm.js
│   │       ├── engine_wasm.d.ts
│   │       └── engine_wasm_bg.wasm
│   └── tests/
├── scripts/
│   └── build-wasm.sh            # cargo build + wasm-bindgen + wasm-opt
└── .github/
    └── workflows/
        └── ci.yml
```

### Pattern 1: Separate Engine and WASM Crates

**What:** The pure Rust engine (`engine`) contains all game logic and type definitions with no WASM dependencies. A thin `engine-wasm` crate wraps it with `#[wasm_bindgen]` exports and tsify type re-exports.

**When to use:** Always, for any Rust project targeting both native and WASM.

**Why:** A single crate cannot have both `crate-type = ["lib"]` and `crate-type = ["cdylib"]` and target both platforms cleanly. The separation also keeps WASM concerns (bindgen, tsify annotations) out of the pure engine code, which enables native testing without WASM overhead.

**Example:**
```rust
// crates/engine/src/types/actions.rs
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "data")]
pub enum GameAction {
    PassPriority,
    PlayLand { card_id: CardId },
    CastSpell { card_id: CardId },
    ActivateAbility { source_id: ObjectId, ability_index: usize },
    DeclareAttackers { attacker_ids: Vec<ObjectId> },
    DeclareBlockers { assignments: Vec<BlockAssignment> },
    // Later phases add variants here
}

// crates/engine-wasm/src/lib.rs
use wasm_bindgen::prelude::*;
use tsify::Tsify;
use engine::types::*;

#[derive(Tsify, Serialize, Deserialize)]
#[tsify(into_wasm_abi, from_wasm_abi)]
#[serde(tag = "type", content = "data")]
pub struct WasmGameAction(GameAction);
// This generates TypeScript:
// export type WasmGameAction =
//   | { type: "PassPriority" }
//   | { type: "PlayLand"; data: { card_id: string } }
//   | ...
```

### Pattern 2: EngineAdapter Interface

**What:** A TypeScript interface that abstracts the transport layer between React components and the Rust engine. Phase 1 implements `WasmAdapter`; Phase 7 adds `TauriAdapter`.

**When to use:** All engine interactions from React go through this adapter.

**Example:**
```typescript
// client/src/adapter/types.ts
export interface EngineAdapter {
  initialize(): Promise<void>;
  submitAction(action: GameAction): Promise<GameEvent[]>;
  getState(): Promise<GameState>;
  dispose(): void;
}

export interface AdapterError {
  code: string;
  message: string;
  recoverable: boolean;
}

// client/src/adapter/wasm-adapter.ts
import init, { process_action, get_state } from '../wasm/engine_wasm';
import type { EngineAdapter, AdapterError } from './types';
import type { GameAction, GameEvent, GameState } from '../wasm/engine_wasm';

export class WasmAdapter implements EngineAdapter {
  private initialized = false;
  private actionQueue: Array<{ action: GameAction; resolve: Function; reject: Function }> = [];
  private processing = false;

  async initialize(): Promise<void> {
    await init();
    this.initialized = true;
  }

  async submitAction(action: GameAction): Promise<GameEvent[]> {
    if (!this.initialized) throw this.createError('NOT_INITIALIZED', 'Adapter not initialized');
    return new Promise((resolve, reject) => {
      this.actionQueue.push({ action, resolve, reject });
      this.processQueue();
    });
  }

  // Queue processing ensures serialized access to WASM
  private async processQueue(): Promise<void> {
    if (this.processing) return;
    this.processing = true;
    while (this.actionQueue.length > 0) {
      const { action, resolve, reject } = this.actionQueue.shift()!;
      try {
        const events = process_action(action);
        resolve(events);
      } catch (e) {
        reject(this.normalizeError(e));
      }
    }
    this.processing = false;
  }

  // ...
}
```

### Pattern 3: Tagged Enum Discriminated Unions

**What:** Using `#[serde(tag = "type", content = "data")]` on Rust enums to produce TypeScript discriminated unions that can be narrowed with `switch(action.type)`.

**When to use:** All shared Rust enums that cross the WASM boundary (GameAction, GameEvent, Zone, Phase, ManaColor).

**Example:**
```rust
// Rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Zone {
    Library,
    Hand,
    Battlefield,
    Graveyard,
    Stack,
    Exile,
    Command,
}
```
```typescript
// Generated TypeScript
export type Zone =
  | { type: "Library" }
  | { type: "Hand" }
  | { type: "Battlefield" }
  | { type: "Graveyard" }
  | { type: "Stack" }
  | { type: "Exile" }
  | { type: "Command" };
```

Note: For simple enums without data (like Zone, ManaColor), `#[serde(tag = "type")]` produces objects. For string-literal unions, use the default serde representation (no tag attribute), which generates `"Library" | "Hand" | "Battlefield" | ...`. Choose based on whether TypeScript consumers need objects or string literals.

### Anti-Patterns to Avoid
- **Putting wasm-bindgen in the engine crate:** Couples pure logic to WASM; prevents native testing and future Tauri native integration
- **Using wasm-pack:** Unmaintained; wraps the same tools but adds complexity and version lag
- **Manual TypeScript type definitions for shared types:** Will drift from Rust; tsify is the single source of truth
- **Passing complex state as JSON strings:** Use serde-wasm-bindgen for native JS value conversion instead of JSON.stringify/parse roundtrips
- **Single monolithic crate with cfg(target_arch):** Creates conditional compilation hell; separate crates are cleaner

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| Rust-to-TS type generation | Manual .d.ts files | tsify derive macro | Types drift; tsify guarantees sync with Rust |
| WASM JS glue code | Manual JS/TS wrappers | wasm-bindgen | Memory management, error handling, type conversion are deceptively complex |
| Persistent data structures | Custom immutable structures | rpds crate | Structural sharing correctness is hard; rpds is battle-tested |
| WASM binary optimization | Custom build scripts | wasm-opt (binaryen) | LLVM output needs post-processing; wasm-opt handles dozens of optimization passes |
| WASM module loading in Vite | Custom async loaders | vite-plugin-wasm | Dev server HMR, production bundling, worker support all handled |
| Version-matched CLI tools | Manual version tracking | Pin wasm-bindgen-cli version to match wasm-bindgen dep | Schema version mismatch causes cryptic build failures |

**Key insight:** The Rust-to-WASM pipeline has many moving parts that must version-match. Pin all `wasm-bindgen*` versions to the same number. The build is: `cargo build` -> `wasm-bindgen` -> `wasm-opt`. Each step transforms the output of the previous one.

## Common Pitfalls

### Pitfall 1: wasm-bindgen Version Mismatch
**What goes wrong:** Build fails with "schema version X but binary version Y" error
**Why it happens:** `wasm-bindgen` (Cargo dep) and `wasm-bindgen-cli` (build tool) must be the exact same version
**How to avoid:** Pin both to the same version (0.2.114). In CI, install the exact version: `cargo install wasm-bindgen-cli --version 0.2.114` or use `cargo-binstall` for speed
**Warning signs:** Any wasm-bindgen error mentioning "schema version"

### Pitfall 2: Missing WASM Target
**What goes wrong:** `cargo build --target wasm32-unknown-unknown` fails with "can't find crate"
**Why it happens:** The wasm32-unknown-unknown target isn't installed by default
**How to avoid:** `rustup target add wasm32-unknown-unknown`. In CI, use `actions-rust-lang/setup-rust-toolchain` with `target: wasm32-unknown-unknown`
**Warning signs:** "error[E0463]: can't find crate for `std`"

### Pitfall 3: tsify Feature Flags
**What goes wrong:** Types generate but don't actually cross the WASM boundary, or generate wrong TS types
**Why it happens:** tsify has two serialization modes: `json` (default, uses serde_json) and `js` (uses serde-wasm-bindgen). The `js` feature is needed for native JsValue conversion
**How to avoid:** Always use `tsify = { version = "0.5", features = ["js"] }` in the WASM crate
**Warning signs:** Runtime errors about "invalid type" or "expected JsValue"

### Pitfall 4: WASM Binary Size Explosion
**What goes wrong:** WASM binary grows to 5-10MB+ with basic types
**Why it happens:** Default release profile doesn't optimize for size; debug info and panic strings included
**How to avoid:** Configure release profile with `opt-level = 'z'`, `lto = true`, `codegen-units = 1`, `strip = true`. Run wasm-opt -Oz post-build
**Warning signs:** WASM file over 1MB for Phase 1's minimal types

### Pitfall 5: Vite Dev vs Build WASM Handling
**What goes wrong:** WASM works in dev mode but fails in production build (or vice versa)
**Why it happens:** Vite dev server and Rollup bundler handle WASM differently
**How to avoid:** Test both `pnpm dev` and `pnpm build && pnpm preview` from the start. Ensure vite-plugin-wasm and vite-plugin-top-level-await are both configured
**Warning signs:** "CompileError: WebAssembly.instantiate()" in production only

### Pitfall 6: Serde Attribute Mismatch Between Crates
**What goes wrong:** Type serialization works in Rust tests but fails at WASM boundary
**Why it happens:** The engine crate defines types with serde attributes, but the WASM crate's tsify wrapper may use different or conflicting serde attributes
**How to avoid:** Define serde attributes ONCE on the engine types. The WASM crate should re-export or newtype-wrap without changing serialization behavior. Consider putting tsify derives directly on engine types (with a feature flag) to avoid duplication
**Warning signs:** "missing field" or "unknown variant" errors at runtime

## Code Examples

### WASM Build Script
```bash
#!/usr/bin/env bash
set -euo pipefail

WASM_OUT="client/src/wasm"
PROFILE="${1:-release}"

# Step 1: Compile Rust to WASM
cargo build --package engine-wasm --target wasm32-unknown-unknown --"$PROFILE"

# Step 2: Generate JS/TS bindings
wasm-bindgen \
  --target web \
  --out-dir "$WASM_OUT" \
  --out-name engine_wasm \
  "target/wasm32-unknown-unknown/$PROFILE/engine_wasm.wasm"

# Step 3: Optimize (release only)
if [ "$PROFILE" = "release" ]; then
  wasm-opt -Oz \
    "$WASM_OUT/engine_wasm_bg.wasm" \
    -o "$WASM_OUT/engine_wasm_bg.wasm"
fi

echo "WASM build complete. Output in $WASM_OUT"
echo "Binary size: $(du -h "$WASM_OUT/engine_wasm_bg.wasm" | cut -f1)"
```

### Cargo Workspace Configuration
```toml
# Cargo.toml (root)
[workspace]
resolver = "2"
members = ["crates/*"]

[workspace.dependencies]
serde = { version = "1", features = ["derive"] }
rpds = { version = "1.2" }

[profile.release]
opt-level = 'z'
lto = true
codegen-units = 1
strip = true
panic = 'abort'
```

### Vite Configuration
```typescript
// client/vite.config.ts
import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import wasm from 'vite-plugin-wasm';
import topLevelAwait from 'vite-plugin-top-level-await';

export default defineConfig({
  plugins: [
    react(),
    wasm(),
    topLevelAwait(),
  ],
  build: {
    target: 'esnext',
  },
});
```

### tsify Type Generation Pattern
```rust
// crates/engine/src/types/mana.rs
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ManaColor {
    White,
    Blue,
    Black,
    Red,
    Green,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ManaPool {
    pub white: u32,
    pub blue: u32,
    pub black: u32,
    pub red: u32,
    pub green: u32,
    pub colorless: u32,
}
```

```rust
// crates/engine-wasm/src/lib.rs
use wasm_bindgen::prelude::*;
use tsify::Tsify;
use serde::{Serialize, Deserialize};

// Re-export engine types with WASM bindings
#[derive(Tsify, Serialize, Deserialize)]
#[tsify(into_wasm_abi, from_wasm_abi)]
pub struct ManaPool(engine::types::ManaPool);

// Simple function to verify WASM integration works
#[wasm_bindgen]
pub fn ping() -> String {
    "forge-ts engine ready".to_string()
}

#[wasm_bindgen]
pub fn create_initial_state() -> JsValue {
    let state = engine::types::GameState::default();
    serde_wasm_bindgen::to_value(&state).unwrap()
}
```

### GitHub Actions CI
```yaml
# .github/workflows/ci.yml
name: CI

on:
  push:
    branches: [main]
  pull_request:
    branches: [main]

jobs:
  rust:
    name: Rust checks
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: actions-rust-lang/setup-rust-toolchain@v1
        with:
          toolchain: stable
          target: wasm32-unknown-unknown
          components: clippy, rustfmt
      - uses: Swatinem/rust-cache@v2

      - name: Format check
        run: cargo fmt --all -- --check
      - name: Clippy
        run: cargo clippy --all-targets -- -D warnings
      - name: Tests
        run: cargo test --all
      - name: Build WASM
        run: cargo build --package engine-wasm --target wasm32-unknown-unknown --release

      - name: Install wasm-bindgen-cli
        run: cargo binstall wasm-bindgen-cli@0.2.114 --no-confirm
      - name: Generate bindings
        run: |
          wasm-bindgen --target web --out-dir wasm-out \
            target/wasm32-unknown-unknown/release/engine_wasm.wasm

      - name: Install wasm-opt
        run: sudo apt-get install -y binaryen
      - name: Optimize WASM
        run: wasm-opt -Oz wasm-out/engine_wasm_bg.wasm -o wasm-out/engine_wasm_bg.wasm

      - name: Report WASM size
        run: |
          SIZE=$(stat -c%s wasm-out/engine_wasm_bg.wasm)
          GZIP_SIZE=$(gzip -c wasm-out/engine_wasm_bg.wasm | wc -c)
          echo "## WASM Binary Size" >> $GITHUB_STEP_SUMMARY
          echo "| Metric | Size |" >> $GITHUB_STEP_SUMMARY
          echo "|--------|------|" >> $GITHUB_STEP_SUMMARY
          echo "| Raw | $(numfmt --to=iec $SIZE) |" >> $GITHUB_STEP_SUMMARY
          echo "| Gzipped | $(numfmt --to=iec $GZIP_SIZE) |" >> $GITHUB_STEP_SUMMARY

  frontend:
    name: Frontend checks
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: pnpm/action-setup@v4
        with:
          version: 9
      - uses: actions/setup-node@v4
        with:
          node-version: 22
          cache: pnpm
          cache-dependency-path: client/pnpm-lock.yaml
      - run: cd client && pnpm install --frozen-lockfile
      - run: cd client && pnpm run lint
      - run: cd client && pnpm run type-check
      - run: cd client && pnpm run test -- --run
```

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| wasm-pack | wasm-bindgen + wasm-bindgen-cli directly | wasm-pack unmaintained since ~2023 | Simpler pipeline, no wrapper overhead |
| tsify-next | tsify (original) | RUSTSEC-2025-0048, Aug 2025 | tsify merged all tsify-next features; tsify-next is deprecated |
| serde_json for WASM boundary | serde-wasm-bindgen | wasm-bindgen official recommendation | Better performance, supports more types |
| Vite 6 | Vite 7.3.x | Late 2025 | Rust-powered, faster builds, Node 20+ required |
| Vitest 3.x | Vitest 4.x | Early 2026 | Stable browser mode, visual regression |
| vite-plugin-top-level-await needed | Still needed (unless target esnext) | Ongoing | WASM ESM integration not yet in Vite core |

**Deprecated/outdated:**
- **wasm-pack:** Unmaintained; wraps wasm-bindgen + wasm-opt but introduces version lag and complexity
- **tsify-next:** Formally deprecated per RUSTSEC-2025-0048; use `tsify` instead
- **serde_json for WASM boundary crossing:** Still works but serde-wasm-bindgen is preferred for performance and type support

## Open Questions

1. **tsify direct on engine types vs wrapper types**
   - What we know: tsify can be used directly on engine types with a feature flag, or via newtype wrappers in the WASM crate
   - What's unclear: Whether putting tsify derives directly on engine types (behind a `wasm` feature) is cleaner than newtype wrappers in engine-wasm
   - Recommendation: Start with direct derives behind a feature flag (`#[cfg_attr(feature = "wasm", derive(Tsify))]`) -- less boilerplate, single source of truth. If it creates compilation issues for native targets, fall back to wrapper types

2. **Zone modeling: typed structs vs enum + map**
   - What we know: MTG has 7 zones with different rules (library is ordered, hand is unordered and hidden, battlefield has permanents with state)
   - What's unclear: Whether zones should be separate typed structs (enforcing zone-specific invariants) or a generic `HashMap<Zone, Vec<ObjectId>>`
   - Recommendation: Use typed structs per zone -- `Library(Vec<CardId>)`, `Hand(HashSet<CardId>)`, `Battlefield(HashMap<ObjectId, Permanent>)` -- because zone rules are fundamentally different and type safety catches bugs at compile time

3. **rpds API coverage for MTG state**
   - What we know: rpds provides List, Vector, HashTrieMap, HashTrieSet with structural sharing
   - What's unclear: Whether rpds covers all needed operations (e.g., efficient indexed access, ordered iteration for library zone)
   - Recommendation: Use rpds for state history/snapshots (undo/redo) rather than as the primary data structure for hot-path game state. Start with standard Rust collections for Phase 1 types; introduce rpds when state management is built in Phase 3

## Validation Architecture

### Test Framework
| Property | Value |
|----------|-------|
| Framework | cargo test (Rust), Vitest 4.x (TypeScript) |
| Config file | None yet -- Wave 0 creates Cargo workspace test config and vitest.config.ts |
| Quick run command | `cargo test --lib` (Rust) / `cd client && pnpm test -- --run` (TS) |
| Full suite command | `cargo test --all && cd client && pnpm test -- --run` |

### Phase Requirements -> Test Map
| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| PLAT-03 | EngineAdapter interface with WasmAdapter | unit | `cd client && pnpm test -- --run src/adapter/` | No -- Wave 0 |
| SC-1 | cargo build produces native binary + WASM module | smoke | `cargo build && cargo build --package engine-wasm --target wasm32-unknown-unknown` | No -- Wave 0 |
| SC-2 | React app imports and calls WASM function | integration | `cd client && pnpm test -- --run src/wasm-integration.test.ts` | No -- Wave 0 |
| SC-3 | Core types exist with TS auto-generation | unit | `cargo test --package engine --lib types` | No -- Wave 0 |
| SC-4 | CI pipeline runs tests and reports WASM size | smoke | Verify `.github/workflows/ci.yml` exists and passes | No -- Wave 0 |

### Sampling Rate
- **Per task commit:** `cargo test --lib && cargo clippy -- -D warnings`
- **Per wave merge:** `cargo test --all && cd client && pnpm test -- --run`
- **Phase gate:** Full suite green before `/gsd:verify-work`

### Wave 0 Gaps
- [ ] Cargo workspace with test configuration (workspace root + crate Cargo.toml files)
- [ ] `client/vitest.config.ts` -- Vitest configuration
- [ ] `client/src/adapter/__tests__/wasm-adapter.test.ts` -- adapter unit tests
- [ ] `crates/engine/src/types/` test modules -- type serialization tests
- [ ] wasm32-unknown-unknown target installed: `rustup target add wasm32-unknown-unknown`

## Sources

### Primary (HIGH confidence)
- [RUSTSEC-2025-0048](https://rustsec.org/advisories/RUSTSEC-2025-0048.html) - tsify-next unmaintained advisory
- [tsify on GitHub](https://github.com/madonoharu/tsify) - v0.5.5, feature documentation
- [wasm-bindgen crates.io](https://crates.io/crates/wasm-bindgen) - v0.2.114 current
- [vite-plugin-wasm npm](https://www.npmjs.com/package/vite-plugin-wasm) - v3.5.0, Vite 2-7 support
- [rpds crates.io](https://crates.io/crates/rpds) - v1.2.0, data structure documentation

### Secondary (MEDIUM confidence)
- [Life after wasm-pack](https://nickb.dev/blog/life-after-wasm-pack-an-opinionated-deconstruction/) - Three-step WASM build pipeline (cargo build -> wasm-bindgen -> wasm-opt)
- [Flinect: Rust WASM with TypeScript Serde](https://flinect.com/blog/rust-wasm-with-typescript-serde) - Tagged enum discriminated union pattern
- [Leptos: Optimizing WASM Binary Size](https://book.leptos.dev/deployment/binary_size.html) - Release profile configuration for WASM size
- [Vite 7 announcement](https://vite.dev/blog/announcing-vite7) - v7.3.x, Node 20+ required
- [Vitest 4 announcement](https://vitest.dev/blog/vitest-4) - v4.0, stable browser mode

### Tertiary (LOW confidence)
- WASM binary size target (<3MB) -- aspirational, actual size depends on included types and dependencies; Phase 1 should be well under 1MB

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH - all libraries verified on crates.io/npm with current versions
- Architecture: HIGH - workspace separation pattern is well-documented Rust+WASM best practice
- Pitfalls: HIGH - version mismatch and build issues are extensively documented in community
- tsify vs tsify-next: HIGH - RUSTSEC advisory is authoritative
- Zone modeling recommendation: MEDIUM - based on MTG domain knowledge, not verified against existing implementations

**Research date:** 2026-03-07
**Valid until:** 2026-04-07 (stable ecosystem, 30-day window)
