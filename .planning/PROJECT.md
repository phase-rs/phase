# Forge.rs — MTG Rules Engine & Game Client

## What This Is

A TypeScript/Rust Magic: The Gathering game engine porting Forge's 32,300+ card definitions. The Rust engine compiles to native (Tauri desktop) and WASM (PWA/browser), with a React frontend featuring full game UI, AI opponent, WebSocket multiplayer, and deck builder. Uses functional architecture (discriminated unions, pure reducers, immutable state) with Forge's card definition format as the upstream compatibility surface.

## Core Value

A player can sit down, pick a Standard-legal deck, and play a full game of Magic against a competent AI opponent — with all cards behaving correctly according to MTG comprehensive rules.

## Requirements

### Validated

<!-- Shipped and confirmed valuable. -->

- ✓ Parse Forge's 32,300+ card definition files (.txt format) into typed Rust structures — v1.0
- ✓ Full MTG turn structure: untap, upkeep, draw, main, combat, main2, end — v1.0
- ✓ Priority system with the stack (LIFO spell/ability resolution) — v1.0
- ✓ State-based actions (0 life, 0 toughness, legend rule, etc.) — v1.0
- ✓ Zone management (library, hand, battlefield, graveyard, stack, exile, command) — v1.0
- ✓ Mana system (5 colors, colorless, hybrid, phyrexian, X costs) — v1.0
- ✓ 202 effect types via handler registry — v1.0
- ✓ 137 trigger types via event bus — v1.0
- ✓ 45 replacement effects — v1.0
- ✓ 61 static ability types with MTG Rule 613 seven-layer evaluation — v1.0
- ✓ Full combat system with keyword interactions — v1.0
- ✓ 50+ keyword abilities — v1.0
- ✓ AI opponent with per-card decision logic and game tree search — v1.0
- ✓ Standard format card coverage (60-70%+ target) — v1.0
- ✓ React game UI: battlefield, hand, stack, targeting, mana payment, card preview — v1.0
- ✓ Touch-optimized responsive design — v1.0
- ✓ Network multiplayer via WebSocket server with hidden information — v1.0
- ✓ Deck builder with card search, filtering, and .dck/.dec import — v1.0
- ✓ Card images from Scryfall API with IndexedDB caching — v1.0
- ✓ Tauri desktop app (Windows, macOS, Linux) — v1.0
- ✓ PWA + WASM build for tablet/browser — v1.0

### Active

<!-- Current scope. Building toward these. -->

(None — next milestone requirements TBD via `/gsd:new-milestone`)

### Out of Scope

<!-- Explicit boundaries. Includes reasoning to prevent re-adding. -->

- Draft/Sealed game modes — significant additional UI/logic, defer to v2
- Quest/Campaign mode — narrative framework not core to gameplay
- Commander/multiplayer formats — adds multiplayer complexity beyond 1v1
- Commercial distribution — open-source project, Scryfall images require non-commercial use
- Direct Java class port — functional architecture chosen over mirroring Forge's OOP hierarchy
- React Native — web technologies (CSS Grid, transforms, Framer Motion) better suited for card game layouts
- Mobile-first — desktop + tablet via PWA; native mobile app via Tauri v2 mobile deferred
- Alchemy/digital-only mechanics — support paper MTG rules only

## Context

Shipped v1.0 with ~29,700 LOC (22.5k Rust + 7.2k TypeScript) across 5 Rust crates and a React frontend.

**Tech stack:** Rust (engine, AI, server) + React/TypeScript/Tailwind v4 (frontend) + Zustand (state) + Framer Motion (animations) + Vite (build) + Tauri v2 (desktop) + Axum (WebSocket server).

**Crate structure:** `engine` (core rules) ← `forge-ai` (AI opponent) ← `engine-wasm` (browser bindings) / `server-core` (session management) ← `forge-server` (Axum WebSocket binary).

**Architecture:** Pure `apply(state, action) -> ActionResult` reducer pattern. Event-driven with discriminated unions across the WASM boundary via serde + tsify. Three Zustand stores (game, UI, animation). Transport-agnostic `EngineAdapter` interface (WASM, Tauri IPC, WebSocket).

**Card format:** Forge's `.txt` card definition format is the upstream compatibility surface — 15+ years stable, 32k+ cards. Parser handles all multi-face types (Split, Transform, MDFC, Adventure).

## Constraints

- **Card format**: Must parse Forge's `.txt` card definition format exactly — non-negotiable for upstream sync
- **Engine language**: Rust — compiles to both native (Tauri) and WASM (PWA) from same source
- **Frontend**: React + TypeScript — rendered in Tauri webview (desktop) or browser (PWA)
- **Desktop wrapper**: Tauri v2 — native performance, small binary, iOS/Android support planned
- **State management**: Immutable state with structural sharing (no object cloning at scale)
- **Card images**: Scryfall API — free for non-commercial, comprehensive, on-demand with caching
- **Build tools**: Cargo (Rust), Vite + pnpm (frontend), Tauri CLI (packaging)
- **Testing**: Rust unit tests for rules engine, Vitest for frontend, CI coverage enforcement
- **Layer system**: Functional evaluation per MTG Rule 613 (no OOP dependency graphs)
- **License**: Open source
- **Code quality**: Clean architecture is paramount — idiomatic Rust/TypeScript, extensible patterns

## Key Decisions

<!-- Decisions that constrain future work. Add throughout project lifecycle. -->

| Decision | Rationale | Outcome |
|----------|-----------|---------|
| Functional port over direct Java port | Card definition format is the compatibility surface, not Java class hierarchy | ✓ Good — Rust enums + pattern matching produced clean, extensible code |
| Rust engine + React frontend | Native performance for AI/rules engine, web flexibility for card game UI | ✓ Good — WASM binary only 19 KB, AI search fast in native Rust |
| Tauri desktop + PWA/WASM tablet | Same engine compiles to native and WASM, thin adapter layer | ✓ Good — EngineAdapter abstraction works cleanly across all 3 transports |
| Forge's .txt card format preserved | 15+ years stable, 32k+ cards, upstream sync is trivial | ✓ Good — parser handles all multi-face types and ability formats |
| Standard format first | Popular format, moderate complexity, 2 years of sets | ✓ Good — focused scope, coverage analysis shows 60%+ achievable |
| Scryfall for card images | Free for non-commercial, comprehensive API | ✓ Good — on-demand with IndexedDB caching, 75ms rate limiting |
| Event bus for triggers (not hardcoded) | 137 trigger types need extensible architecture | ✓ Good — per-call registry build is cheap and avoids static patterns |
| Tree/DAG effect representation | Conditional ability chains need branching | ✓ Good — SVar resolution with sub-ability chaining handles complex cards |
| HashMap<ObjectId, GameObject> central store | Zones as Vec<ObjectId> with central lookup | ✓ Good — simple, fast, avoids ownership complexity |
| ChaCha20Rng for cross-platform determinism | StdRng not guaranteed same across platforms | ✓ Good — WASM and native produce identical sequences from same seed |
| fn pointer effect/trigger/static registries | Built per apply() call, cheap HashMap | ✓ Good — simple, no trait objects or global state |
| petgraph for layer dependency ordering | Seven-layer system needs topological sort | ✓ Good — handles cycles with fallback to timestamp ordering |

---
*Last updated: 2026-03-08 after v1.0 milestone*
