# Forge.ts — MTG Rules Engine & Game Client

## What This Is

A TypeScript/Rust port of MTG Forge — an open-source Magic: The Gathering game engine with 32,300+ card definitions. The engine is written in Rust (compiling to both native and WASM), with a React frontend served via Tauri (desktop) or as a PWA (tablets). It preserves Forge's card definition format for upstream compatibility while using a functional architecture (discriminated unions, pure reducers, immutable state) instead of Forge's Java class hierarchy.

## Core Value

A player can sit down, pick a Standard-legal deck, and play a full game of Magic against a competent AI opponent — with all cards behaving correctly according to MTG comprehensive rules.

## Requirements

### Validated

<!-- Shipped and confirmed valuable. -->

(None yet — ship to validate)

### Active

<!-- Current scope. Building toward these. -->

- [ ] Parse Forge's 32,300+ card definition files (.txt format) into typed Rust structures
- [ ] Full MTG turn structure: untap, upkeep, draw, main, combat, main2, end
- [ ] Priority system with the stack (LIFO spell/ability resolution)
- [ ] State-based actions (0 life, 0 toughness, legend rule, etc.)
- [ ] Zone management (library, hand, battlefield, graveyard, stack, exile, command)
- [ ] Mana system (5 colors, colorless, hybrid, phyrexian, X costs)
- [ ] 202 effect types via handler registry (Draw, DealDamage, ChangeZone, Pump, Destroy, Counter, Token, etc.)
- [ ] 137 trigger types via event bus (ETB, dies, attacks, spell cast, damage dealt, etc.)
- [ ] 45 replacement effects (damage prevention, redirect, ETB modifications, etc.)
- [ ] 61 static ability types with MTG Rule 613 seven-layer evaluation system
- [ ] Full combat system (attack, block, first strike, double strike, trample, deathtouch, lifelink, etc.)
- [ ] 50+ keyword abilities (flying, haste, hexproof, ward, flashback, kicker, cycling, etc.)
- [ ] Forge-level AI (~57k LOC equivalent) with per-card decision logic and game tree search
- [ ] Standard format card coverage (last 2 years of sets, targeting 60-70%+ coverage)
- [ ] React game UI: battlefield, hand, stack, phase tracker, targeting, mana payment, card preview
- [ ] Touch-optimized responsive design (great on tablets)
- [ ] Network multiplayer via WebSocket server (hidden information handled server-side)
- [ ] Deck builder with card search and filtering
- [ ] Import .dck/.dec deck files from Forge
- [ ] Card images from Scryfall API (on-demand loading with local cache)
- [ ] Tauri desktop app (Windows, macOS, Linux)
- [ ] PWA + WASM build for tablet/browser access (same React UI, engine compiled to WASM)

### Out of Scope

<!-- Explicit boundaries. Includes reasoning to prevent re-adding. -->

- Draft/Sealed game modes — significant additional UI/logic, defer to v2
- Quest/Campaign mode — narrative framework not core to gameplay
- Commander/multiplayer formats — adds multiplayer complexity beyond 1v1
- Commercial distribution — open-source project, Scryfall images require non-commercial use
- Direct Java class port — functional architecture chosen over mirroring Forge's OOP hierarchy
- React Native — web technologies (CSS Grid, transforms, Framer Motion) better suited for card game layouts
- Mobile-first — desktop + tablet via PWA; native mobile app via Tauri v2 mobile deferred

## Context

**Forge** is a ~480k LOC Java MTG implementation maintained for 15+ years with a stable card definition format. The card `.txt` format is the upstream compatibility surface — new cards and errata from Forge sync directly. Rule changes map to specific handler functions by the *principle of the change* (e.g., "new trigger type X" → add X case to trigger handler).

**Alchemy** exists at `../alchemy` as a simplified card game with a proven pure-function reducer architecture. Its patterns (Zustand store, discriminated union actions, event-driven state) inform the design philosophy but the Rust engine will be purpose-built for MTG's complexity.

**Key architectural insight**: Forge's Java relies on deep inheritance (SpellAbility → SpellAbilityBase → SpellApiBased → AbilitySub) and mutable god objects (Card.java is 3000+ lines). Porting 1:1 would produce unidiomatic code. The card definition parser is the compatibility layer; the engine uses Rust enums, pattern matching, and immutable state.

**Performance considerations**: MTG game state is 10-50x larger than simple card games. AI game tree search benefits enormously from native Rust performance. Structural sharing (not object cloning) is required for state management. The engine needs a tree/DAG effect representation for conditional ability chains and a proper event bus for 137 trigger types.

## Constraints

- **Card format**: Must parse Forge's `.txt` card definition format exactly — non-negotiable for upstream sync
- **Engine language**: Rust — compiles to both native (Tauri) and WASM (PWA) from same source
- **Frontend**: React + TypeScript — rendered in Tauri webview (desktop) or browser (PWA)
- **Desktop wrapper**: Tauri v2 — native performance, small binary, iOS/Android support planned
- **State management**: Immutable state with structural sharing (no object cloning at scale)
- **Card images**: Scryfall API — free for non-commercial, comprehensive, on-demand with caching
- **Build tools**: Cargo (Rust), Vite + pnpm (frontend), Tauri CLI (packaging)
- **Testing**: Rust unit tests + property-based tests for rules engine, Vitest for frontend, Cypress for E2E
- **Layer system**: Functional evaluation per MTG Rule 613 (no OOP dependency graphs)
- **License**: Open source
- **Code quality**: Clean architecture is paramount — idiomatic Rust/TypeScript, extensible patterns, no shortcuts that compromise maintainability. Every abstraction must earn its place.

## Key Decisions

<!-- Decisions that constrain future work. Add throughout project lifecycle. -->

| Decision | Rationale | Outcome |
|----------|-----------|---------|
| Functional port over direct Java port | Card definition format is the compatibility surface, not Java class hierarchy. Rust enums + pattern matching > deep inheritance | — Pending |
| Rust engine + React frontend | Native performance for AI/rules engine, web flexibility for complex card game UI layouts | — Pending |
| Tauri desktop + PWA/WASM tablet | Same Rust engine compiles to native (Tauri IPC) and WASM (browser). Same React UI, thin adapter layer. Easy iPad install without App Store | — Pending |
| Forge's .txt card format preserved | 15+ years stable, 32k+ cards, upstream sync is trivial for card-only releases | — Pending |
| Standard format first | Popular format, moderate complexity, 2 years of sets — good balance of coverage effort vs. player interest | — Pending |
| Scryfall for card images | Free for non-commercial, comprehensive API, on-demand loading with local cache | — Pending |
| Event bus for triggers (not hardcoded) | 137 trigger types need extensible architecture, not per-type handler wiring | — Pending |
| Tree/DAG effect representation | Conditional ability chains ("if X, do Y, otherwise Z") need branching, not linear array | — Pending |

---
*Last updated: 2026-03-07 after initialization*
