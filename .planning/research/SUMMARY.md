# Project Research Summary

**Project:** Forge.rs -- MTG Rules Engine & Game Client
**Domain:** Game engine port (Java to Rust/TypeScript) with dual-target desktop/PWA delivery
**Researched:** 2026-03-07
**Confidence:** HIGH

## Executive Summary

Forge.rs is a port of the Java-based Forge MTG client to a Rust rules engine with a React/TypeScript frontend, targeting both native desktop (Tauri) and browser/tablet (WASM PWA) from a single codebase. The expert approach for this domain is a platform-agnostic pure-function engine using persistent data structures for cheap state cloning (critical for AI tree search), wrapped by thin platform adapters. The Rust ecosystem is mature enough for this: `rpds` for structural sharing, `serde` + `wasm-bindgen` for cross-boundary serialization, and Tauri v2 for the desktop shell. The recommended architecture -- immutable state, enum-based dispatch, command-buffer mutations -- is both idiomatic Rust and a natural fit for MTG's discrete event-driven rules.

The primary risk is rules engine complexity, not technology. MTG has 202 effect types, 137 trigger modes, a 7-layer continuous effect system with intra-layer dependencies, and recursive state-based action checks. These are the systems that have wrecked previous MTG engine projects. The mitigation is to build bottom-up (types, then zones, then stack, then abilities, then triggers, then layers), validate each level with ported Forge tests, and defer the long tail of card coverage until the architecture proves sound. The secondary risk is WASM binary size -- without aggressive optimization from day one, the PWA target becomes unusable.

The recommended approach is: build the pure Rust engine first with no UI, prove correctness through property-based testing and ported Forge test cases, then add Tauri/WASM bridges and the React UI in parallel. The adapter pattern (Zustand store consuming an EngineAdapter interface) means the same React components work identically whether backed by Tauri IPC or direct WASM calls. Start with a Standard-format MVP (~500-1000 playable cards) to validate the architecture before scaling to full card coverage.

## Key Findings

### Recommended Stack

The stack splits cleanly into three layers: a pure Rust engine crate with zero platform dependencies, thin platform adapter crates (Tauri binary and WASM cdylib), and a React/TypeScript frontend. See STACK.md for full details.

**Core technologies:**
- **Rust (stable 1.85+):** Engine language -- compiles to native and WASM from single codebase, ownership model prevents state bugs, pattern matching ideal for MTG's closed type sets
- **rpds:** Persistent data structures with structural sharing -- cheap cloning for AI game tree search (replaces unmaintained `im` crate)
- **Tauri 2.x:** Desktop shell -- small binaries, fast IPC via channels, future mobile support
- **wasm-bindgen + wasm-opt:** WASM toolchain -- replaces archived wasm-pack, generates JS bindings and optimizes binary size
- **React 19 + Zustand 5 + Vite 6:** Frontend -- component model fits card game UI, Zustand's external-store pattern works with the adapter layer
- **proptest:** Property-based testing -- generates random game states to validate engine invariants across effect/trigger combinations
- **tsify-next:** Auto-generates TypeScript types from Rust structs -- keeps frontend types in sync without manual maintenance

**Critical version pins:** wasm-bindgen crate and wasm-bindgen-cli MUST match versions (0.2.114). Mismatches cause cryptic build failures.

### Expected Features

See FEATURES.md for full competitive analysis and dependency graph.

**Must have (table stakes):**
- Correct rules engine (turns, priority, stack, zones, state-based actions, mana system)
- Combat system with core keywords (flying, trample, first strike, deathtouch, lifelink, haste, vigilance, reach, menace)
- Top 15 effect types covering ~60% of cards (Draw, DealDamage, ChangeZone, Pump, Destroy, Counter, Token, etc.)
- Basic trigger system (ETB, dies, attacks, spell cast, damage dealt)
- Card image display via Scryfall API with caching
- Battlefield, hand, stack, phase tracker, life totals, targeting UI, mana payment UI
- AI opponent (heuristic: play lands, cast best spell, attack when favorable)
- Deck builder with card search and .dck import
- Game log

**Should have (differentiators):**
- Native + WASM dual target from single codebase
- Smart auto-tap (context-aware, better than Arena's)
- Fast AI via Rust performance
- Offline-first PWA
- Touch-optimized UI (no MTG client does tablet well)

**Defer to v2+:**
- Draft/Sealed modes, Quest/Adventure mode, Commander/multiplayer
- Advanced AI (deep tree search, per-card logic)
- Animations, sound, custom UI layouts
- Sideboard/best-of-3

### Architecture Approach

The architecture follows a strict separation: a pure-function engine crate that takes actions and returns new state plus events, wrapped by platform-specific adapter crates. The engine uses enum-based dispatch (not trait objects) for all closed type sets, persistent data structures for immutable state, and a command-buffer pattern where effect handlers return mutations rather than mutating state directly. See ARCHITECTURE.md for component boundaries, data flow diagrams, and the full Cargo workspace layout.

**Major components:**
1. **forge-engine** (lib crate) -- Pure rules engine: GameState, ActionDispatcher, ZoneManager, TurnManager, StackManager, ManaSystem, AbilitySystem, EffectHandlers (202 types), TriggerSystem, ReplacementSystem, StaticAbilitySystem (layers), CombatSystem, StateBasedActions, CardParser, AI
2. **forge-tauri** (bin crate) -- Thin Tauri wrapper: holds engine state in Arc<Mutex<>>, exposes #[tauri::command] functions, emits events via channels, runs AI on background thread
3. **forge-wasm** (cdylib crate) -- WASM bridge: #[wasm_bindgen] exports mirroring Tauri commands, holds state in WASM linear memory, calls JS callbacks for events
4. **React frontend** -- EngineAdapter interface (TauriAdapter/WasmAdapter), Zustand GameStore, game UI components (Battlefield, HandDisplay, StackDisplay, PromptBar, CardPreview)

**Key anti-patterns to avoid:** God-object GameState with methods, trait objects for effect dispatch, mutable in-place state updates, leaking Tauri/WASM into engine crate, separate frontend/backend state models.

### Critical Pitfalls

See PITFALLS.md for all 15 pitfalls with detailed prevention strategies.

1. **Layer system dependency cycles (Rule 613)** -- Effects within the same layer can depend on each other, overriding timestamp ordering. Implement dependency detection as a separate pass before applying effects within each layer. Port Forge's layer tests as the initial test suite.
2. **State-based actions recursive loop** -- SBAs must be checked in a fixpoint loop (check, apply all atomically, recheck). Add a loop counter cap (~1000) and treat exceeding it as a draw per Rule 104.4b.
3. **Replacement effect self-application** -- Each replacement can only modify a given event once. Track per-event application with unique event IDs. Player chooses order when multiple replacements apply.
4. **Rust ownership vs. game state mutation** -- Effects need to read state while producing mutations. Use the command-buffer pattern: handlers take &GameState and return Vec<StateMutation>, never &mut GameState.
5. **WASM binary size explosion** -- Target <3MB for engine WASM. Configure release profile (panic=abort, opt-level=z, lto=true, strip=true), use wasm-opt, profile with twiggy, set CI size budget from day one.

## Implications for Roadmap

Based on combined research, the project naturally splits into 8 phases following the dependency graph identified in ARCHITECTURE.md and validated by PITFALLS.md phase warnings.

### Phase 1: Project Scaffold & Core Types
**Rationale:** Everything depends on the type system. Enum definitions for actions, events, zones, phases, and mana drive every subsequent component. Must also establish the dual-target build (native + WASM) from day one to catch threading/platform issues early.
**Delivers:** Cargo workspace with three crates, Vite + React frontend skeleton, CI pipeline with WASM size tracking, core type definitions (GameState, GameAction, GameEvent, Zone, Phase, ManaColor enums)
**Addresses:** Project structure, build tooling
**Avoids:** Pitfall 9 (WASM threading -- design async interface upfront), Pitfall 5 (WASM size -- set budget from first build)

### Phase 2: Card Parser & Database
**Rationale:** Card definitions are the data the engine operates on. The parser is independently testable against Forge's existing card files and validates the type system from Phase 1.
**Delivers:** Parser for Forge .txt card format, CardDefinition structs, CardDatabase index, ability string parsing foundation
**Addresses:** Card parser (table stakes), deck import foundation
**Avoids:** Pitfall 7 (multi-face card parsing -- build test matrix of all face types)

### Phase 3: Game State Engine (Zones, Mana, Turns, Stack, SBAs)
**Rationale:** The core game loop must work before any card abilities. Two players should be able to take turns, play lands, tap for mana, pass priority, and have SBAs checked. This validates the dispatch/action/event architecture.
**Delivers:** Working game loop with land play, mana production, phase progression, priority passing, stack operation, state-based action checking
**Addresses:** Turn/phase system, priority, mana system, zone management (all table stakes)
**Avoids:** Pitfall 2 (SBA recursive loop -- atomic batch checking), Pitfall 4 (ownership model -- command buffer pattern), Pitfall 14 (mana payment -- constraint satisfaction)

### Phase 4: Ability System & Top 15 Effects
**Rationale:** This is the riskiest phase -- if the ability parsing and effect handler architecture is wrong, everything downstream breaks. Target: cast Lightning Bolt, Counterspell, Giant Growth, create tokens, and see them resolve correctly.
**Delivers:** Ability parser, effect handler registry, top 15 ApiType handlers (~60% card coverage), targeting system with legality rechecks
**Addresses:** Core spellcasting, targeting UI requirements
**Avoids:** Pitfall 12 (targeting legality rechecks -- store IDs, revalidate on resolution)

### Phase 5: Triggers & Combat
**Rationale:** Triggers depend on the ability system (triggered abilities are abilities). Combat depends on triggers (attack/block triggers) and the stack (combat damage uses the stack in some contexts). This phase unlocks creature-based gameplay.
**Delivers:** Event bus, trigger matching by mode, APNAP ordering, full combat system with core keywords, death triggers, ETB triggers
**Addresses:** Combat system, trigger system (table stakes)
**Avoids:** Pitfall 10 (APNAP ordering -- bake into trigger architecture)

### Phase 6: Advanced Rules (Replacements, Layers, Static Abilities)
**Rationale:** Most rules-complex phase. Replacement effects modify the event pipeline before triggers fire. The layer system (Rule 613) requires careful dependency handling. Fewer cards depend on correct layer evaluation for basic play, so this can come after combat.
**Delivers:** Replacement effect pipeline with per-event tracking, 7-layer continuous effect evaluation with dependency detection, static ability grants
**Addresses:** Static abilities (table stakes for full rules correctness)
**Avoids:** Pitfall 1 (layer dependency cycles -- dependency detection pass, port Forge tests), Pitfall 3 (replacement self-application -- unique event IDs)

### Phase 7: Platform Bridges & React UI
**Rationale:** The engine API stabilizes after Phase 6. Bridge crates are thin wrappers. React UI development can start earlier with mock data, but full integration happens here. The adapter pattern means UI code is platform-agnostic.
**Delivers:** Tauri commands and event emission, WASM exports, EngineAdapter with TauriAdapter/WasmAdapter, full game UI (battlefield, hand, stack, prompt bar, card preview, phase tracker, life totals), Scryfall image loading with three-tier cache, deck builder
**Addresses:** All UI table stakes, card image display, deck builder, dual-target delivery
**Avoids:** Pitfall 6 (IPC bottleneck -- batch updates at priority windows, delta state), Pitfall 11 (card image loading -- three-tier cache), Pitfall 15 (serde overhead -- opaque handles where possible), Pitfall 13 (webview CSS -- test cross-platform early)

### Phase 8: AI & Card Coverage Expansion
**Rationale:** AI requires a working rules engine to enumerate legal actions and evaluate board states. Card coverage expansion (remaining 187 effect types) is parallelizable and independent. Both build on the stable foundation from prior phases.
**Delivers:** Heuristic AI (board evaluation, land play, spell casting, combat decisions), remaining effect handlers by card frequency, Standard format card coverage target
**Addresses:** AI opponent (table stakes), card coverage (table stakes for playability)
**Avoids:** Pitfall 8 (game tree explosion -- heuristic-first, no deep tree search for v1)

### Phase Ordering Rationale

- **Types before behavior:** Enum definitions drive everything; wrong types mean rewriting all downstream code
- **Parser before engine:** Card definitions are the data the engine operates on; validates the type system
- **Stack before abilities:** Spells and abilities go on the stack; stack must work first
- **Abilities before triggers:** Triggers execute abilities; the ability system must be proven before adding trigger dispatch
- **Triggers before replacements:** Replacements modify the event pipeline that triggers consume; ordering matters
- **Layers after combat:** Most cards play correctly without perfect layer evaluation; combat is higher priority for playability
- **UI can parallel from Phase 3 onward:** React components only need zone state data, not full rules, to start rendering
- **AI is last:** It needs everything else to work; heuristic AI is sufficient for MVP

### Research Flags

Phases likely needing deeper research during planning:
- **Phase 4 (Ability System):** Highest-risk phase. The mapping from Forge's Java ability/effect class hierarchy to Rust enums + handler functions needs careful design. Research Forge's SpellAbility class hierarchy and ApiType taxonomy in detail.
- **Phase 6 (Replacements & Layers):** Most complex rules interactions. Research MTG Comprehensive Rules 613 (layers) and 614 (replacement effects) directly. Port Forge's test cases as specification.
- **Phase 7 (Platform Bridges):** Tauri v2 Channel API and WASM boundary optimization need hands-on prototyping. The IPC protocol design (full state vs. deltas vs. opaque handles) should be validated with profiling.

Phases with standard patterns (skip research-phase):
- **Phase 1 (Scaffold):** Well-documented Cargo workspace + Tauri + Vite setup. Official Tauri templates exist.
- **Phase 2 (Card Parser):** Straightforward text parsing against known format. Forge card files are the spec.
- **Phase 3 (Game State Engine):** The reducer/dispatcher pattern is well-established. MTG turn structure is well-documented in the Comprehensive Rules.
- **Phase 5 (Triggers & Combat):** Event bus + observer pattern is standard. Combat rules are well-specified.
- **Phase 8 (AI):** Forge's existing AI architecture (heuristic-based, ~57k LOC) provides the blueprint. Start simple.

## Confidence Assessment

| Area | Confidence | Notes |
|------|------------|-------|
| Stack | HIGH | All recommended technologies are actively maintained, well-documented, and have clear rationale. Version pins are specific. Only medium-confidence items (rpds, Tailwind, Framer Motion) are swappable without architectural impact. |
| Features | HIGH | Competitive landscape is well-understood. Feature dependencies are mapped. MVP scope is clearly defined with rational deferral decisions. |
| Architecture | HIGH | Pattern is validated by Argentum engine, Rust community consensus, and the existing Forge port plan. Component boundaries follow natural dependency lines. |
| Pitfalls | HIGH | All critical pitfalls cite specific MTG rules, documented failure modes, or confirmed platform limitations. Prevention strategies are concrete and actionable. |

**Overall confidence:** HIGH

### Gaps to Address

- **rpds vs im crate in practice:** STACK.md recommends rpds (actively maintained) but ARCHITECTURE.md examples use `im` crate syntax. Decision is rpds, but verify its API supports all needed operations (HashMap, Vector, OrdMap equivalents) during Phase 1 scaffold.
- **tsify-next compatibility:** MEDIUM confidence. Must verify it works with current wasm-bindgen 0.2.114 and generates correct discriminated union types for the GameAction/GameEvent enums. Test during Phase 1.
- **Tauri Channel vs Event performance:** ARCHITECTURE.md suggests starting with events and migrating to channels if needed. Validate during Phase 7 prototyping -- MTG state updates may or may not hit the throughput threshold where channels matter.
- **Forge card format completeness:** The parser must handle all multi-face card types (Split, Flip, Transform, Meld, Adventure, MDFC, Aftermath, Fuse). Exact format conventions need validation against actual Forge card files during Phase 2.
- **WASM binary size budget feasibility:** The <3MB target for the engine WASM is aspirational. With 202 effect handlers compiled in, actual size needs measurement during Phase 1. May need to lazy-load effect handlers or split the WASM module.

## Sources

### Primary (HIGH confidence)
- [Tauri v2 Documentation](https://v2.tauri.app/) -- IPC, commands, events, channels, state management
- [wasm-bindgen Guide](https://rustwasm.github.io/docs/wasm-bindgen/) -- WASM-JS bridge, serde integration
- [MTG Comprehensive Rules](https://magic.wizards.com/en/rules) -- Authoritative rules reference (layers, SBAs, replacements, priority, triggers)
- [Forge GitHub](https://github.com/Card-Forge/forge) -- Source reference for card format, AI architecture, rules engine patterns
- [Rust WASM Size Optimization Guide](https://rustwasm.github.io/book/reference/code-size.html) -- Official size reduction techniques

### Secondary (MEDIUM confidence)
- [Argentum MTG Engine Architecture](https://wingedsheep.com/building-argentum-a-magic-the-gathering-rules-engine/) -- Validated architecture pattern (deterministic engine, state projection)
- [rpds GitHub](https://github.com/orium/rpds) -- Persistent data structures (actively maintained replacement for im crate)
- [serde-wasm-bindgen docs](https://docs.rs/serde-wasm-bindgen/latest/serde_wasm_bindgen/) -- Direct JS value conversion for WASM boundary
- [tsify-next GitHub](https://github.com/AmbientRun/tsify-next) -- TypeScript type generation from Rust
- [Forge Rules Engine Blog](http://mtgrares.blogspot.com/2009/12/rules-engine-is-pain-in-neck.html) -- Original developer's perspective on complexity

### Tertiary (LOW confidence)
- [tauri-interop crate](https://lib.rs/crates/tauri-interop) -- Generate WASM functions from Tauri commands (may simplify adapter layer, needs evaluation)
- WASM binary size <3MB target -- aspirational based on documented techniques, needs validation with actual engine code

---
*Research completed: 2026-03-07*
*Ready for roadmap: yes*
