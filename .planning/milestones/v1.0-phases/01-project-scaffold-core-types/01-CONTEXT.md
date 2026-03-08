# Phase 1: Project Scaffold & Core Types - Context

**Gathered:** 2026-03-07
**Status:** Ready for planning

<domain>
## Phase Boundary

Working build system that compiles Rust to both native and WASM from the same source, a React frontend skeleton that imports and calls WASM bindings, core type definitions (GameState, GameAction, GameEvent, Zone, Phase, ManaColor + foundation types), and CI pipeline. Requirement: PLAT-03 (EngineAdapter abstraction).

</domain>

<decisions>
## Implementation Decisions

### WASM binding strategy
- wasm-pack is dead/unmaintained — use `wasm-bindgen` + `wasm-bindgen-cli` directly
- Vite imports WASM via `vite-plugin-wasm` — no intermediate tooling
- All Rust-to-TypeScript type generation via `tsify-next` — single source of truth, zero manual TS types for shared engine types
- Manual TypeScript types only for frontend-only concerns (UI state, component props)

### EngineAdapter (PLAT-03)
- Rich adapter interface — async queuing, error normalization, command pattern
- React components call `adapter.submitAction(action)` without knowing the transport
- Phase 1 implements WasmAdapter only — TauriAdapter deferred to Phase 7
- Interface designed to support both transports from day one

### Core type scope
- Foundation types beyond success criteria: Player, CardId/ObjectId, CardDefinition (stub), Color, ManaPool, AbilityType (enum variants)
- Gives Phase 2-4 a stable foundation to build on without restructuring

### Action/Event model
- GameAction and GameEvent are Rust enums (discriminated unions) from day one
- tsify-next generates TypeScript discriminated unions automatically
- Phase 1 defines top-level variants; later phases add data to each variant
- Exhaustive pattern matching over trait objects — aligns with functional architecture

### Claude's Discretion
- **Zone modeling**: Claude picks the approach that best fits MTG's zone rules (typed zone structs vs enum + map)
- **Forge awareness in Phase 1 types**: Claude decides whether CardDefinition stub mirrors Forge field names or stays abstract
- **Workspace layout**: Idiomatic Cargo workspace structure — user wants best-practice clean architecture
- **CI pipeline**: GitHub Actions with best-practice checks (tests, clippy, formatting, WASM size reporting)
- **React app structure**: Standard Vite + React + TypeScript setup

</decisions>

<specifics>
## Specific Ideas

- User emphasized "most idiomatic" and "clean architecture" repeatedly — prioritize conventional Rust/TS patterns over clever solutions
- User wants long-term value from architectural choices — design for the 8-phase journey, not just Phase 1
- wasm-pack deprecation should be documented as a constraint so future phases don't accidentally reference it

</specifics>

<code_context>
## Existing Code Insights

### Reusable Assets
- No existing code — greenfield project

### Established Patterns
- Reference project `../forge` (Java) — card definition format is the compatibility surface
- Reference project `../alchemy` — proven pure-function reducer architecture (Zustand, discriminated unions, event-driven state)

### Integration Points
- Forge card `.txt` files at `../forge` for Phase 2 parser testing
- Alchemy patterns for state management approach inspiration

</code_context>

<deferred>
## Deferred Ideas

None — discussion stayed within phase scope

</deferred>

---

*Phase: 01-project-scaffold-core-types*
*Context gathered: 2026-03-07*
