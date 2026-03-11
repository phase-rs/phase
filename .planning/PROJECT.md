# phase.rs — MTG Rules Engine & Game Client

## What This Is

A Rust/TypeScript Magic: The Gathering game engine with MTGJSON card data and custom typed ability definitions, featuring an MTGA-quality React frontend. The Rust engine compiles to native (Tauri desktop) and WASM (PWA/browser), featuring art-crop card presentation, cinematic animations, audio, AI opponent, WebSocket multiplayer, and deck builder. Uses functional architecture (discriminated unions, pure reducers, immutable state) with 100% Standard-legal card coverage.

## Core Value

A player can sit down, pick a Standard-legal deck, and play a full game of Magic against a competent AI opponent — with all cards behaving correctly according to MTG comprehensive rules.

## Requirements

### Validated

<!-- Shipped and confirmed valuable. -->

- ✓ Load card data from MTGJSON (MIT) metadata + typed JSON ability definitions — v1.0, v1.2
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
- ✓ MTGA-quality game board with responsive CSS custom property card sizing — v1.1
- ✓ Canvas particle VFX and step-based animation queue with event normalizer — v1.1
- ✓ Web Audio API sound effects (39 SFX) and WUBRG-themed background music — v1.1
- ✓ AI auto-play game loop with auto-pass heuristics and opponent controller abstraction — v1.1
- ✓ Stack visualization, smart mana auto-pay, combat assignment, and priority controls — v1.1
- ✓ Combat evasion keywords, Ward, Protection, Wither/Infect, Prowess mechanics — v1.1
- ✓ MTGA-faithful art-crop cards, golden targeting arcs, cinematic turn banners, death shatter — v1.1
- ✓ Mode-first menu flow, deck gallery with art tiles, splash screen — v1.1
- ✓ Mana abilities (Rule 605), equipment/aura attachment, Scry/Dig/Surveil interactive choices — v1.1
- ✓ Planeswalker loyalty abilities, DFC transform, day/night, morph/manifest — v1.1
- ✓ 100% Standard-legal card coverage with CI gate preventing regressions — v1.1

### Active

<!-- Current scope. Building toward these. -->

- [x] MTGJSON integration — pull card metadata from MTGJSON's MIT-licensed JSON — v1.2
- [x] Own ability format — typed JSON schema for abilities/triggers/effects mapping to Rust types — v1.2
- [x] Card format migration — convert 78 curated Standard cards to new format — v1.2
- [x] Remove Forge coupling — remove data/cardsfolder/, make Forge parser optional/dev-only — v1.2
- [x] License change — relicense as MIT/Apache-2.0 dual license — v1.2
- [ ] Test suite — comprehensive rules correctness tests using XMage MIT scenarios as reference
- [x] Update project constraints — remove Forge format dependency from key decisions — v1.2

### Out of Scope

<!-- Explicit boundaries. Includes reasoning to prevent re-adding. -->

- Draft/Sealed game modes — significant additional UI/logic, defer to v2
- Quest/Campaign mode — narrative framework not core to gameplay
- Commander/multiplayer formats — adds multiplayer complexity beyond 1v1
- Commercial distribution — open-source project, Scryfall images require non-commercial use
- Direct OOP class hierarchy port — functional architecture chosen over mirroring traditional MTG engine OOP patterns
- React Native — web technologies (CSS Grid, transforms, Framer Motion) better suited for card game layouts
- Mobile-first — desktop + tablet via PWA; native mobile app via Tauri v2 mobile deferred
- Alchemy/digital-only mechanics — support paper MTG rules only
- Tutorial system — defer to future milestone
- Foil effects / element card effects — visual polish, defer to future milestone

## Context

Shipped v1.1 with ~51,500 LOC (33.4k Rust + 18.1k TypeScript) across 5 Rust crates and a React frontend.

**Tech stack:** Rust (engine, AI, server) + React/TypeScript/Tailwind v4 (frontend) + Zustand (state) + Framer Motion (animations) + Web Audio API (audio) + Vite (build) + Tauri v2 (desktop) + Axum (WebSocket server).

**Crate structure:** `engine` (core rules) ← `phase-ai` (AI opponent) ← `engine-wasm` (browser bindings) / `server-core` (session management) ← `phase-server` (Axum WebSocket binary).

**Architecture:** Pure `apply(state, action) -> ActionResult` reducer pattern. Event-driven with discriminated unions across the WASM boundary via serde + tsify. Three Zustand stores (game, UI, animation) + preferences store. Transport-agnostic `EngineAdapter` interface (WASM, Tauri IPC, WebSocket). AudioManager singleton with Web Audio API.

**Card format:** MTGJSON (MIT) for card metadata + own typed JSON ability definitions. Card file parser available behind `forge-compat` feature gate for development/migration use.

**Standard coverage:** 100% of curated 78-card Standard-legal subset supported, enforced by CI gate.

## Constraints

- **Card format**: MTGJSON (MIT) for card metadata + own typed JSON ability format — legacy card file parser behind `forge-compat` feature gate
- **Engine language**: Rust — compiles to both native (Tauri) and WASM (PWA) from same source
- **Frontend**: React + TypeScript — rendered in Tauri webview (desktop) or browser (PWA)
- **Desktop wrapper**: Tauri v2 — native performance, small binary, iOS/Android support planned
- **State management**: Immutable state with structural sharing (no object cloning at scale)
- **Card images**: Scryfall API — free for non-commercial, comprehensive, on-demand with caching
- **Build tools**: Cargo (Rust), Vite + pnpm (frontend), Tauri CLI (packaging)
- **Testing**: Rust unit tests for rules engine, Vitest for frontend, CI coverage enforcement
- **Layer system**: Functional evaluation per MTG Rule 613 (no OOP dependency graphs)
- **License**: MIT OR Apache-2.0 dual license
- **Code quality**: Clean architecture is paramount — idiomatic Rust/TypeScript, extensible patterns

## Key Decisions

<!-- Decisions that constrain future work. Add throughout project lifecycle. -->

| Decision | Rationale | Outcome |
|----------|-----------|---------|
| Functional architecture over OOP port | Discriminated unions and pattern matching over class hierarchies | ✓ Good — Rust enums + pattern matching produced clean, extensible code |
| Rust engine + React frontend | Native performance for AI/rules engine, web flexibility for card game UI | ✓ Good — WASM binary only 19 KB, AI search fast in native Rust |
| Tauri desktop + PWA/WASM tablet | Same engine compiles to native and WASM, thin adapter layer | ✓ Good — EngineAdapter abstraction works cleanly across all 3 transports |
| MTGJSON + typed ability JSON | MIT-licensed data source, typed schema with schemars validation | ✓ Good — clean licensing, typed definitions, JSON pipeline |
| Standard format first | Popular format, moderate complexity, 2 years of sets | ✓ Good — 100% Standard coverage achieved in v1.1 |
| Scryfall for card images | Free for non-commercial, comprehensive API | ✓ Good — dual-size (art_crop + normal) with IndexedDB caching |
| Event bus for triggers (not hardcoded) | 137 trigger types need extensible architecture | ✓ Good — per-call registry build is cheap and avoids static patterns |
| Tree/DAG effect representation | Conditional ability chains need branching | ✓ Good — SVar resolution with sub-ability chaining handles complex cards |
| HashMap<ObjectId, GameObject> central store | Zones as Vec<ObjectId> with central lookup | ✓ Good — simple, fast, avoids ownership complexity |
| ChaCha20Rng for cross-platform determinism | StdRng not guaranteed same across platforms | ✓ Good — WASM and native produce identical sequences from same seed |
| fn pointer effect/trigger/static registries | Built per apply() call, cheap HashMap | ✓ Good — simple, no trait objects or global state |
| petgraph for layer dependency ordering | Seven-layer system needs topological sort | ✓ Good — handles cycles with fallback to timestamp ordering |
| Port Alchemy UI as Arena-style frontend | Alchemy has polished game UI with clean architecture matching phase.rs patterns | ✓ Good — MTGA-quality board, animations, and audio shipped in v1.1 |
| Preserve EngineAdapter abstraction during UI port | Keeps WASM + Tauri + WebSocket support without coupling to specific transport | ✓ Good — all 3 transports work with Arena UI |
| Art-crop + normal dual image strategy | Battlefield needs compact art crops, hand/stack needs full card images | ✓ Good — cache hits from shared Scryfall URLs, visual fidelity matches MTGA |
| AudioManager as plain TypeScript singleton | Matches dispatch.ts pattern, no React lifecycle coupling | ✓ Good — clean integration with animation pipeline via setTimeout offsets |
| Step-based animation queue with event normalizer | Groups related game events into visual steps for smooth playback | ✓ Good — configurable speed, VFX quality levels, death creature persistence |
| BackFaceData for symmetric DFC transform | Both faces preserved for unlimited round-trip transforms | ✓ Good — reused for morph/manifest face-down mechanics |
| Standard card curation by name (not set code) | Name-based matching works across printings | ✓ Good — 78 cards, 79 faces, CI gate prevents regressions |
| MIT/Apache-2.0 dual license | Removes GPL coupling from Forge card data, follows Rust ecosystem convention | ✓ Good — clean IP with MTGJSON (MIT) as sole external data source |

## Current Milestone: v1.2 Migrate Data Source & Add Tests

**Goal:** Migrate to MTGJSON (MIT-licensed) + custom ability format, add comprehensive test coverage, and relicense as MIT/Apache-2.0.

**Target features:**
- MTGJSON integration for card metadata
- Own typed JSON ability/trigger/effect schema
- Migration of 78 curated Standard cards
- Remove Forge data dependency (parser optional/dev-only)
- MIT/Apache-2.0 relicensing
- Comprehensive rules correctness test suite (XMage MIT reference)

---
*Last updated: 2026-03-11 after v1.2 relicensing complete*
