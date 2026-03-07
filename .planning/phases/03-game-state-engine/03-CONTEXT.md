# Phase 3: Game State Engine - Context

**Gathered:** 2026-03-07
**Status:** Ready for planning

<domain>
## Phase Boundary

Two players can take turns through a full MTG turn cycle — untap, draw, play lands, tap for mana, pass priority, stack resolution (LIFO), and have state-based actions enforced. London mulligan for game start. This phase builds the core game loop; abilities beyond basic land mana production and spell casting mechanics are Phase 4.

</domain>

<decisions>
## Implementation Decisions

### State Management
- Standard Rust collections (Vec, HashMap, BTreeMap) — defer rpds/persistent data structures until profiling justifies it (Phase 8 AI earliest)
- Mutate-in-place: `fn apply(&mut GameState, action) -> Vec<GameEvent>` — matches Forge, idiomatic Rust with ownership safety
- Events returned from apply — caller decides what to do with them (trigger system in Phase 5, UI updates in Phase 7)
- Guiding principle: follow Forge's approach unless Rust has a genuinely better alternative

### Turn/Priority Model
- Action-response pattern: engine processes one action, returns `(state, events, WaitingFor)` — no internal game loop
- Engine auto-advances through phases that need no player input (untap → upkeep → draw → stop at main phase) in a single apply call
- `WaitingFor` enum tells caller what input is needed next — extensible for future phases (targets, cost payment, trigger ordering)
- Priority counter: track consecutive passes. Both pass in succession → empty stack advances phase, non-empty stack resolves top
- State-based actions run as fixpoint loop after every action (match MTG rules 704.3 and Forge's implementation)

### Zone Storage
- Per-zone collections on Player: library (Vec<ObjectId>), hand, graveyard
- Shared top-level collections: battlefield, stack, exile
- Central `HashMap<ObjectId, GameObject>` for actual object data
- Seeded RNG (rand crate StdRng) for library shuffling — deterministic for testing, replays, and network play

### GameObject Model
- Port all rules-relevant fields from Forge's Card.java — not display/UI fields
- Includes: tapped, face_down, flipped, controller, owner, counters, damage_marked, attached_to, attachments, power, toughness, abilities, keywords, zone
- Full model built upfront so the struct stabilizes across phases 4-6
- Skip: art, foil state, UI preferences, display-only fields (~50 rules fields vs ~200 total in Forge)

### Mana System
- Full payment system in Phase 3 (not deferred to Phase 4)
- ManaPool restructured: track individual mana units with source ObjectId and restrictions (matches Forge's Mana class)
- Add, spend, clear operations — clear pool on phase transitions per MTG rules
- Hybrid/phyrexian cost payment: auto-select best payment (prefer color with most available), player can override via explicit ManaPayment action
- Supports: 5 colors, colorless, generic, hybrid, phyrexian, X costs, snow

### Claude's Discretion
- Auto-advance behavior: Claude decides whether to advance through multiple phases in one call or step-by-step (both approaches discussed, user deferred)
- Internal module organization for the game engine
- Exact WaitingFor enum variants
- SBA implementation order and which SBAs to implement in Phase 3 vs later
- How to represent the stack (simple Vec<StackEntry> vs more complex)
- London mulligan flow details (how bottom-of-library choice is presented)

</decisions>

<specifics>
## Specific Ideas

- "Most decisions should be derived from how Forge does something unless Rust has a better implementation alternative" — Forge parity is the north star, consistent across all phases
- Full Card model and full mana payment upfront — user prefers building complete subsystems rather than incremental stubs
- Action-response pattern chosen specifically because it works perfectly with WASM (can't block), AI game tree search (branch on each possible action), and React UI (dispatch action, get new state)

</specifics>

<code_context>
## Existing Code Insights

### Reusable Assets
- `GameState` struct with turn_number, active_player, phase, players, priority_player (crates/engine/src/types/game_state.rs)
- `Phase` enum with all 12 MTG turn phases (crates/engine/src/types/phase.rs)
- `Zone` enum with all 7 MTG zones (crates/engine/src/types/zones.rs)
- `GameAction` enum with PassPriority, PlayLand, CastSpell, MulliganDecision, etc. (crates/engine/src/types/actions.rs)
- `GameEvent` enum with TurnStarted, PhaseChanged, PriorityPassed, ZoneChanged, etc. (crates/engine/src/types/events.rs)
- `ManaPool` with add/total, `ManaColor` enum, `ManaCostShard` with all 40+ variants (crates/engine/src/types/mana.rs)
- `Player` with id, life, mana_pool (crates/engine/src/types/player.rs)
- `CardId` and `ObjectId` newtypes (crates/engine/src/types/identifiers.rs)
- `CardDatabase` with name indexing and face-level lookup (crates/engine/src/database/)

### Established Patterns
- All types derive `Debug, Clone, Serialize, Deserialize`
- Tagged union serialization: `#[serde(tag = "type", content = "data")]`
- Newtype wrappers for type safety (CardId, ObjectId, PlayerId)
- Cargo workspace: `crates/engine` (core) and `crates/engine-wasm` (bindings)

### Integration Points
- `GameState` needs expansion: add zones, objects map, stack, seeded RNG
- `Player` needs expansion: add per-player zone collections (library, hand, graveyard)
- `ManaPool` needs restructuring: from simple counters to tracked mana units with source/restrictions
- `GameAction`/`GameEvent` enums may need new variants for Phase 3 actions
- Engine logic goes in new module (e.g., `crates/engine/src/game/`) alongside existing `types/`, `parser/`, `database/`

</code_context>

<deferred>
## Deferred Ideas

None — discussion stayed within phase scope

</deferred>

---

*Phase: 03-game-state-engine*
*Context gathered: 2026-03-07*
