# Architecture Patterns

**Domain:** MTG rules engine with Rust backend, React frontend, Tauri/WASM dual targets
**Researched:** 2026-03-07

## Recommended Architecture

### High-Level System Diagram

```
+-------------------------------------------------------------------+
|                        Cargo Workspace                             |
|                                                                    |
|  +---------------------+   +------------------+   +-----------+   |
|  |  forge-engine (lib)  |   |  forge-tauri     |   | forge-wasm|   |
|  |  Pure rules engine   |   |  (bin)           |   | (cdylib)  |   |
|  |  No I/O, no platform |   |  Tauri commands  |   | wasm-     |   |
|  |  dependencies        |   |  Event emission  |   | bindgen   |   |
|  |                      |   |  State holder    |   | exports   |   |
|  +----------+-----------+   +--------+---------+   +-----+-----+   |
|             |                        |                   |         |
|             +------------------------+-------------------+         |
|             |           depends on forge-engine           |        |
+-------------------------------------------------------------------+
              |                        |
              v                        v
    +------------------+     +------------------+
    |  Tauri IPC        |     | WASM bridge      |
    |  Commands/Events  |     | wasm-bindgen     |
    |  Channels         |     | serde-wasm-      |
    +--------+----------+     | bindgen          |
             |                +--------+---------+
             v                         v
    +------------------------------------------------+
    |            React Frontend (Vite + TS)           |
    |                                                 |
    |  Zustand store  <-- adapter -->  Engine API     |
    |  React components                               |
    |  Scryfall images                                |
    +------------------------------------------------+
```

### Core Principle: Platform-Agnostic Engine

The engine crate (`forge-engine`) is a pure Rust library with **zero platform dependencies**. No `std::fs`, no `std::net`, no Tauri imports, no `wasm-bindgen` annotations. It takes input (actions), produces output (new state + events), and nothing else. This is the single most important architectural decision -- it enables the dual-target strategy and makes the engine trivially testable.

**Confidence: HIGH** -- This pattern is well-established in the Rust ecosystem. The Argentum MTG engine uses an identical separation. The `im` crate for persistent data structures and `serde` for serialization work identically on native and WASM targets.

---

## Component Boundaries

### 1. forge-engine (lib crate)

The rules engine. Pure functions, no side effects, no I/O.

| Component | Responsibility | Communicates With |
|-----------|---------------|-------------------|
| **GameState** | Immutable game state struct (zones, battlefield, stack, players, phase, priority) | All engine components read it |
| **ActionDispatcher** | `fn dispatch(state: &GameState, action: GameAction) -> DispatchResult` | Receives actions, calls subsystems, returns new state + events |
| **ZoneManager** | Card movement between zones (library, hand, battlefield, graveyard, stack, exile, command) | ActionDispatcher, TriggerSystem |
| **TurnManager** | Phase/step progression, priority passing, active player tracking | ActionDispatcher |
| **StackManager** | LIFO stack for spells/abilities, resolution, priority after resolution | ActionDispatcher, AbilitySystem |
| **ManaSystem** | Mana pool management, cost payment, mana emptying on phase change | ActionDispatcher, AbilitySystem |
| **AbilitySystem** | Parse Forge ability strings, resolve effects via handler registry | StackManager, EffectHandlers |
| **EffectHandlers** | Registry of `ApiType -> handler fn` (202 effect types) | AbilitySystem, GameState |
| **TriggerSystem** | Event bus: match GameEvents against registered triggers, put triggered abilities on stack | ActionDispatcher (post-event), StackManager |
| **ReplacementSystem** | Intercept pending events, apply replacement effects before they happen | ActionDispatcher (pre-event) |
| **StaticAbilitySystem** | Layer 1-7 evaluation per MTG Rule 613, produces DerivedState | Queried on demand for "what does the board actually look like" |
| **CombatSystem** | Attack/block declaration, damage assignment, keyword interactions | ActionDispatcher, TriggerSystem |
| **StateBasedActions** | Check-and-apply after each priority pass (0 life, 0 toughness, legend rule, etc.) | ActionDispatcher |
| **CardParser** | Parse Forge `.txt` files into `CardDefinition` structs | Loaded at startup, feeds AbilitySystem |
| **CardDatabase** | Index of all parsed card definitions by name | AbilitySystem, AI |

### 2. forge-tauri (bin crate)

Thin Tauri wrapper. Holds engine state, exposes commands, emits events.

| Component | Responsibility | Communicates With |
|-----------|---------------|-------------------|
| **GameManager** | Holds `Arc<Mutex<GameState>>`, manages game lifecycle | Tauri commands |
| **Tauri Commands** | `#[tauri::command]` functions: `new_game`, `submit_action`, `get_state`, `get_legal_actions` | Frontend via IPC |
| **Event Emitter** | Pushes `GameEvent`s to frontend via `app.emit()` or `Channel` | Frontend listeners |
| **AI Runner** | Runs AI on background thread, submits actions back to GameManager | GameManager |

### 3. forge-wasm (cdylib crate)

WASM bridge. Exposes same API as Tauri commands via `wasm-bindgen`.

| Component | Responsibility | Communicates With |
|-----------|---------------|-------------------|
| **WASM Exports** | `#[wasm_bindgen]` functions mirroring Tauri commands | Frontend via JS calls |
| **State Holder** | Holds `GameState` in WASM linear memory | WASM exports |
| **Event Callback** | Calls JS callback with serialized events | Frontend event handler |

### 4. React Frontend

Same codebase for Tauri and PWA. Adapter pattern switches between IPC and WASM calls.

| Component | Responsibility | Communicates With |
|-----------|---------------|-------------------|
| **EngineAdapter** | Interface: `submitAction()`, `getState()`, `getLegalActions()`, `onEvent()` | Either Tauri IPC or WASM module |
| **TauriAdapter** | Implements EngineAdapter via `invoke()` and `listen()` | Tauri backend |
| **WasmAdapter** | Implements EngineAdapter via direct WASM function calls | forge-wasm module |
| **GameStore** (Zustand) | Holds frontend game state, derived from engine state snapshots | EngineAdapter, React components |
| **GamePage** | Top-level game layout | All UI components |
| **Battlefield** | Renders permanents in grid, tap state, counters, attachments | GameStore |
| **HandDisplay** | Fan of cards with legal-play highlighting | GameStore |
| **StackDisplay** | Visual stack of spells/abilities resolving | GameStore |
| **PromptBar** | Priority actions, targeting mode, choice dialogs | GameStore, EngineAdapter |
| **CardPreview** | Large card image on hover/click | Scryfall image loader |

---

## Data Flow

### Action Flow: UI -> Engine -> State Update -> UI Render

```
1. User clicks "Cast Lightning Bolt targeting opponent"
       |
       v
2. React PromptBar builds GameAction::CastSpell { card_id, targets, mana_paid }
       |
       v
3. EngineAdapter.submitAction(action)
       |
       +-- Tauri path: invoke('submit_action', { action })
       |       |
       |       v
       |   Tauri command deserializes, calls engine.dispatch()
       |
       +-- WASM path: wasmModule.submit_action(serialized_action)
               |
               v
           WASM export deserializes, calls engine.dispatch()
       |
       v
4. Engine dispatch pipeline:
   a. Validate action legality
   b. Check replacement effects (ReplacementSystem)
   c. Apply action (put spell on stack, pay costs)
   d. Emit GameEvents (SpellCast, ManaSpent, ZoneChanged)
   e. Check triggers against emitted events (TriggerSystem)
   f. Put triggered abilities on stack
   g. Run state-based actions (StateBasedActions)
   h. Return DispatchResult { new_state, events }
       |
       v
5. Bridge layer sends response:
       +-- Tauri: app.emit("game_events", events) + command returns new state
       +-- WASM: returns serialized { state, events } to JS
       |
       v
6. Frontend receives:
   a. GameStore updates with new state snapshot
   b. Event handler processes GameEvents for animations
   c. React re-renders affected components
```

### State Query Flow (Layer System)

```
Component needs "what is this creature's power?"
       |
       v
StaticAbilitySystem.evaluate(base_state) -> DerivedState
       |
       v
Layer 1: Copy effects (Clone, etc.)
Layer 2: Control changes (Mind Control, etc.)
Layer 3: Text changes (rare)
Layer 4: Type changes (Arcane Adaptation, etc.)
Layer 5: Color changes
Layer 6: Ability add/remove (Muraganda Petroglyphs, etc.)
Layer 7: P/T modifications
  7a: Characteristic-defining abilities (Tarmogoyf)
  7b: Set P/T (Turn to Frog "0/1")
  7c: Modifications from +1/+1 and -1/-1 counters
  7d: Static P/T modifiers (Glorious Anthem "+1/+1")
  7e: Switching P/T
       |
       v
DerivedState contains final computed values for all permanents
```

### Priority Flow

```
Action resolves or phase begins
       |
       v
State-based actions checked (loop until none apply)
       |
       v
Triggered abilities placed on stack (active player orders theirs)
       |
       v
Active player receives priority
       |
       v
Player has priority:
  +-- Takes action -> resolve, repeat from top
  +-- Passes -> next player receives priority
         +-- All players pass in succession:
              +-- Stack empty -> advance phase/step
              +-- Stack non-empty -> resolve top of stack, repeat from top
```

---

## Patterns to Follow

### Pattern 1: Enum-Based Effect Dispatch (not trait objects)

**What:** Use Rust enums with pattern matching for all closed type sets (actions, events, effects, triggers, zones, phases). Use `enum_dispatch` crate for trait-like ergonomics with enum performance.

**Why:** MTG has a closed set of ~202 effect types, ~137 trigger modes, ~45 replacement events. These are known at compile time. Enum dispatch is ~10x faster than trait objects and enables exhaustive pattern matching (compiler catches missing cases when new variants are added).

**Confidence: HIGH** -- Rust community consensus for closed type sets. `enum_dispatch` crate benchmarks confirm performance advantage.

**When:** Every type that maps to a Forge taxonomy (ApiType, TriggerMode, ReplacementEvent, Zone, Phase, Color).

**Example:**

```rust
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GameAction {
    PlayLand { card_id: CardInstanceId },
    CastSpell { card_id: CardInstanceId, targets: Vec<Target>, mana_paid: ManaCost },
    ActivateAbility { permanent_id: PermanentId, ability_index: usize, targets: Vec<Target> },
    DeclareAttackers { assignments: Vec<AttackAssignment> },
    DeclareBlockers { assignments: Vec<BlockAssignment> },
    PassPriority,
    PayMana { color: ManaColor, source_id: PermanentId },
    MakeMulliganChoice(MulliganChoice),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GameEvent {
    CardDrawn { player_id: PlayerId, card_id: CardInstanceId },
    ZoneChanged { card_id: CardInstanceId, from: Zone, to: Zone },
    DamageDealt { source_id: ObjectId, target_id: ObjectId, amount: u32 },
    LifeChanged { player_id: PlayerId, old: i32, new: i32 },
    SpellResolved { card_id: CardInstanceId },
    // ... exhaustive list
}

// Effect handler dispatch -- pattern matching, not vtable
pub fn handle_effect(state: &GameState, effect: &ResolvedEffect) -> EffectResult {
    match &effect.api_type {
        ApiType::Draw => handlers::draw(state, effect),
        ApiType::DealDamage => handlers::deal_damage(state, effect),
        ApiType::ChangeZone => handlers::change_zone(state, effect),
        ApiType::Pump => handlers::pump(state, effect),
        // compiler error if you miss one
    }
}
```

### Pattern 2: Immutable State with `im` Crate

**What:** Use the `im` crate's persistent data structures (`HashMap`, `Vector`, `OrdMap`) for game state. These provide O(log n) structural sharing on clone -- cloning a state with 1000 permanents copies a few pointers, not 1000 objects.

**Why:** The AI needs to clone game states thousands of times during tree search. Full deep clone is O(n) and kills performance. `im` collections share structure, making clone O(1) and modification O(log n).

**Confidence: HIGH** -- `im` is the standard Rust crate for this (6M+ downloads). Works on WASM target. The Argentum engine uses this exact pattern.

**Example:**

```rust
use im::{HashMap, Vector};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameState {
    pub players: HashMap<PlayerId, PlayerState>,
    pub zones: HashMap<(PlayerId, Zone), Vector<CardInstanceId>>,
    pub battlefield: Vector<Permanent>,
    pub stack: Vector<StackEntry>,
    pub phase: Phase,
    pub active_player: PlayerId,
    pub priority_player: PlayerId,
    pub turn: u32,
    // Clone is cheap -- structural sharing
}
```

### Pattern 3: Deterministic Engine with No I/O

**What:** `fn dispatch(state: &GameState, action: GameAction) -> DispatchResult` is a pure function. No randomness (RNG seed is part of state), no I/O, no system calls.

**Why:** Deterministic engines are trivially testable (feed state + action, assert output), replayable (record actions, replay for debugging), and network-syncable (both sides run same reducer on same actions).

**Confidence: HIGH** -- This is the architecture the port plan specifies, Argentum uses it, and it's a well-known pattern in game engine design.

**Example:**

```rust
pub struct DispatchResult {
    pub state: GameState,
    pub events: Vec<GameEvent>,
    pub pending_choices: Option<PlayerChoice>,  // engine needs player input
}

pub fn dispatch(state: &GameState, action: GameAction) -> Result<DispatchResult, IllegalAction> {
    // 1. Validate
    let legal = get_legal_actions(state);
    if !legal.contains(&action) {
        return Err(IllegalAction { action, reason: "not legal" });
    }
    // 2. Apply (may check replacements, emit events, check triggers, run SBAs)
    let result = apply_action(state, action);
    Ok(result)
}
```

### Pattern 4: Adapter Pattern for Tauri/WASM Bridge

**What:** Define a TypeScript interface `EngineAdapter` that both `TauriAdapter` and `WasmAdapter` implement. The React app codes against the interface, never against a specific backend.

**Why:** Same React codebase runs in Tauri (desktop) and browser (PWA). The adapter hides whether the engine is a Rust process (IPC) or a WASM module (direct calls).

**Confidence: HIGH** -- Standard dependency inversion. The `tauri-interop` crate exists specifically for this pattern.

**Example:**

```typescript
// Shared interface
interface EngineAdapter {
  newGame(config: GameConfig): Promise<GameState>;
  submitAction(action: GameAction): Promise<DispatchResult>;
  getState(): Promise<GameState>;
  getLegalActions(): Promise<GameAction[]>;
  onEvent(callback: (event: GameEvent) => void): () => void;  // returns unsubscribe
}

// Tauri implementation
class TauriAdapter implements EngineAdapter {
  async submitAction(action: GameAction): Promise<DispatchResult> {
    return invoke('submit_action', { action });
  }
  onEvent(callback: (event: GameEvent) => void) {
    const unlisten = listen<GameEvent>('game_event', (e) => callback(e.payload));
    return () => { unlisten.then(fn => fn()); };
  }
}

// WASM implementation
class WasmAdapter implements EngineAdapter {
  private engine: typeof import('forge-wasm');
  async submitAction(action: GameAction): Promise<DispatchResult> {
    return this.engine.submit_action(action);  // direct WASM call
  }
  onEvent(callback: (event: GameEvent) => void) {
    this.engine.set_event_callback(callback);
    return () => this.engine.clear_event_callback();
  }
}
```

### Pattern 5: Tauri Channels for Game State Streaming

**What:** Use `tauri::ipc::Channel<T>` for pushing state updates from Rust to frontend, instead of the event system.

**Why:** Tauri's event system evaluates JavaScript directly and is not designed for high-throughput scenarios. Channels provide ordered, fast delivery and avoid JSON serialization overhead. For a game where state updates happen rapidly (especially during AI turns), channels are the correct choice.

**Confidence: MEDIUM** -- Tauri v2 docs explicitly recommend channels over events for high-throughput. However, game state updates may not be frequent enough to matter. Start with events (simpler), migrate to channels if performance requires it.

**Example:**

```rust
#[tauri::command]
async fn start_game(
    config: GameConfig,
    state_channel: tauri::ipc::Channel<GameStateSnapshot>,
    event_channel: tauri::ipc::Channel<Vec<GameEvent>>,
    state: tauri::State<'_, GameManager>,
) -> Result<(), String> {
    let mut game = state.new_game(config);
    state_channel.send(game.snapshot()).map_err(|e| e.to_string())?;

    // AI loop runs on this thread, pushing updates via channel
    while !game.is_over() {
        if game.current_player_is_ai() {
            let action = ai::choose_action(&game);
            let result = game.dispatch(action);
            event_channel.send(result.events).map_err(|e| e.to_string())?;
            state_channel.send(game.snapshot()).map_err(|e| e.to_string())?;
        } else {
            break;  // wait for player action via separate command
        }
    }
    Ok(())
}
```

### Pattern 6: Shared Types via tsify + serde

**What:** Use `#[derive(Tsify, Serialize, Deserialize)]` on all types shared between Rust and TypeScript. The `tsify` crate auto-generates `.d.ts` files and implements WASM ABI conversions.

**Why:** Eliminates manual type synchronization between Rust and TypeScript. The generated TypeScript types match Rust enum variants exactly, including discriminated unions for `GameAction`, `GameEvent`, etc.

**Confidence: MEDIUM** -- `tsify` is well-established (160+ dependents) but the Tauri path uses `serde_json` serialization via IPC, which doesn't use `tsify`'s WASM bindings. You'll need `tsify` for the WASM target and may want to generate types separately for the Tauri target (or just share the `.d.ts` output). This is a minor ergonomic friction, not a blocker.

---

## Anti-Patterns to Avoid

### Anti-Pattern 1: God Object GameState with Methods

**What:** Putting behavior on `GameState` (e.g., `game_state.cast_spell(...)`, `game_state.deal_damage(...)`).

**Why bad:** Creates a massive struct with hundreds of methods. Impossible to test individual subsystems in isolation. Mirrors Forge's `Card.java` (3000+ lines) problem.

**Instead:** `GameState` is a data-only struct. Behavior lives in free functions or system modules that take `&GameState` and return new state. The `dispatch()` function is the single entry point.

### Anti-Pattern 2: Trait Objects for Effect Handlers

**What:** `Box<dyn EffectHandler>` stored in a `HashMap<ApiType, Box<dyn EffectHandler>>`.

**Why bad:** Dynamic dispatch overhead (10x slower than enum match), no exhaustive matching (can't catch missing handlers at compile time), harder to serialize for WASM boundary crossing.

**Instead:** Enum-based dispatch with `match`. The `ApiType` enum has 202 variants; the compiler enforces handling every variant.

### Anti-Pattern 3: Mutable State with In-Place Updates

**What:** `game_state.battlefield.push(permanent)`, `player.life -= 3`.

**Why bad:** AI tree search needs thousands of state clones. Mutable state requires deep cloning (expensive). Also makes it impossible to implement undo or replay.

**Instead:** `im` crate persistent collections. Clone is O(1), modification creates new version sharing structure with original.

### Anti-Pattern 4: Leaking Tauri/WASM into Engine

**What:** Using `#[tauri::command]` or `#[wasm_bindgen]` annotations in the engine crate. Using `AppHandle` in engine code.

**Why bad:** Makes the engine crate platform-dependent. Can't compile it for WASM if it imports Tauri. Can't test it without a Tauri runtime.

**Instead:** Engine crate has zero platform dependencies. Thin adapter crates (`forge-tauri`, `forge-wasm`) wrap the engine API.

### Anti-Pattern 5: Separate State on Frontend and Backend

**What:** Frontend maintains its own game state model, backend maintains another, and they sync via deltas.

**Why bad:** State divergence bugs. Two sources of truth. Complex reconciliation logic.

**Instead:** Engine is the single source of truth. Frontend receives complete state snapshots (or partial views for hidden information). Frontend state is a cache of the last engine snapshot, not an independent model.

---

## Cargo Workspace Structure

```
forge.rs/
  Cargo.toml                    # workspace root
  crates/
    forge-engine/
      Cargo.toml                # [lib], no platform deps
      src/
        lib.rs
        state/
          mod.rs                # GameState, PlayerState, Permanent, etc.
          zones.rs              # Zone management
          mana.rs               # ManaPool, ManaCost
        actions/
          mod.rs                # GameAction enum, dispatch()
          validation.rs         # Legal action enumeration
        effects/
          mod.rs                # ApiType enum, handler registry
          handlers/             # One file per handler group
            draw.rs
            damage.rs
            change_zone.rs
            pump.rs
            ...
        triggers/
          mod.rs                # TriggerMode enum, event matching
          modes/                # Per-mode matching logic
        replacements/
          mod.rs                # ReplacementEvent enum
        statics/
          mod.rs                # StaticMode enum
          layers.rs             # 7-layer evaluation
        combat/
          mod.rs                # Combat phases, damage assignment
        stack.rs                # Stack management
        turn.rs                 # Phase/step progression, priority
        sba.rs                  # State-based actions
        parser/
          mod.rs                # Forge .txt file parser
          ability.rs            # Ability string parser
          cost.rs               # Cost string parser
          target.rs             # Target restriction parser
          card_types.rs         # Type line parser
        card_db.rs              # Card database index
        ai/
          mod.rs                # AI entry point
          heuristics.rs         # Board evaluation
          search.rs             # Game tree search

    forge-tauri/
      Cargo.toml                # [bin], depends on forge-engine + tauri
      src/
        main.rs                 # Tauri builder setup
        commands.rs             # #[tauri::command] functions
        game_manager.rs         # State holder, AI thread management
        events.rs               # Event emission helpers

    forge-wasm/
      Cargo.toml                # [lib] crate-type = ["cdylib"], depends on forge-engine + wasm-bindgen
      src/
        lib.rs                  # #[wasm_bindgen] exports mirroring Tauri commands

  frontend/                     # React + Vite + TypeScript
    src/
      adapters/
        engine-adapter.ts       # EngineAdapter interface
        tauri-adapter.ts        # Tauri IPC implementation
        wasm-adapter.ts         # WASM bridge implementation
      stores/
        game-store.ts           # Zustand store
      components/
        game/
          GamePage.tsx
          Battlefield.tsx
          HandDisplay.tsx
          StackDisplay.tsx
          PromptBar.tsx
          PhaseTracker.tsx
          ManaDisplay.tsx
          CardPreview.tsx
      types/
        generated/              # Auto-generated from Rust via tsify
          game-state.d.ts
          actions.d.ts
          events.d.ts
```

---

## Scalability Considerations

| Concern | At MVP (100 cards) | At Standard (2000 cards) | At Full (32k cards) |
|---------|-------------------|-------------------------|---------------------|
| Card loading | Load all into memory (~5MB) | Load all (~50MB), fine for desktop | Lazy-load by format/set, index in memory (~200MB) |
| State cloning (AI) | `im` handles trivially | `im` handles well | `im` handles well -- structural sharing scales |
| Layer evaluation | Fast (few permanents) | Memoize per-layer results, invalidate on change | Same + consider incremental evaluation |
| Trigger checking | Linear scan of battlefield triggers | Index triggers by event type for O(1) lookup | Same indexed approach |
| WASM memory | No concern | ~100MB budget fine | May need to lazy-load card definitions |
| Serialization | JSON fine | JSON fine | Consider MessagePack/bincode for large state transfers |
| AI search depth | 3-4 ply easy | 2-3 ply with pruning | Same -- complexity is per-board-state, not per-card-count |

---

## Suggested Build Order (Based on Component Dependencies)

The dependency graph dictates build order. Each layer depends only on layers below it.

```
Level 0 (no deps):     CardParser, Core Types (GameState, enums)
Level 1 (needs L0):    ZoneManager, ManaSystem, TurnManager
Level 2 (needs L1):    StackManager, StateBasedActions
Level 3 (needs L2):    AbilitySystem core (parser + top 15 effect handlers)
Level 4 (needs L3):    TriggerSystem, CombatSystem
Level 5 (needs L4):    ReplacementSystem, StaticAbilitySystem (layers)
Level 6 (needs L3):    Remaining effect handlers (long tail, parallelizable)
Level 7 (needs L2):    Tauri bridge, WASM bridge (thin wrappers)
Level 8 (needs L7):    React UI (can start with mock data at L0)
Level 9 (needs L3+L8): AI foundation
```

### Recommended Phase Structure

1. **Foundation** -- Core types + card parser + zone/mana/turn basics (Levels 0-1)
   - No external dependencies to validate
   - Establishes type system everything else builds on
   - Card parser is independently testable against Forge's card files

2. **Stack & Priority** -- Stack, SBAs, basic game loop (Level 2)
   - Two players can take turns, play lands, pass priority
   - Validates the dispatch/action/event architecture

3. **Ability Core** -- Ability parser + top 15 effects (Level 3)
   - This is the riskiest phase: if the ability parsing architecture is wrong, everything breaks
   - Target: cast Lightning Bolt, Counterspell, Giant Growth, and see them resolve
   - **First playable milestone** when combined with basic UI

4. **Triggers & Combat** -- Event bus + combat system (Level 4)
   - ETB triggers, dies triggers, combat triggers
   - Full combat with keywords

5. **Advanced Rules** -- Replacements + layer system (Level 5)
   - Most rules-complex phase
   - Replacement effects intercept the event pipeline
   - Layer system requires careful ordering

6. **Bridge + UI** -- Tauri/WASM adapters + React frontend (Levels 7-8)
   - Can start UI work earlier with mock data
   - Bridge layer is thin -- a few days of work once engine API is stable

7. **Coverage & AI** -- Remaining effects + AI + card coverage push (Levels 6, 9)
   - Long tail of effect handlers is parallelizable
   - AI builds on stable engine

### Why This Order

- **Types before behavior**: Enum definitions drive everything; get them right first
- **Parser before engine**: Card definitions are the data the engine operates on
- **Stack before abilities**: Abilities go on the stack; stack must work first
- **Abilities before triggers**: Triggers execute abilities; ability system must work first
- **Triggers before replacements**: Replacement effects modify the trigger/event pipeline
- **Layers last in engine**: Most complex, fewest cards depend on it for basic play
- **UI can parallel**: React development can start with mock data alongside engine work

---

## How Forge's Java Maps to Idiomatic Rust

| Java Pattern (Forge) | Rust Pattern | Rationale |
|----------------------|-------------|-----------|
| `SpellAbility` class hierarchy (6 levels deep) | `ParsedAbility` struct + `ApiType` enum | Rust enums replace inheritance. No need for `SpellAbilityBase`, `SpellApiBased`, etc. -- a flat struct with an enum discriminator captures all the info |
| `Card.java` (3000+ LOC god object) | `CardDefinition` (parsed template) + `CardInstance` (runtime) + `Permanent` (on battlefield) | Split by lifecycle: definition is immutable, instance tracks zone/modifications, permanent adds battlefield-specific state |
| `ApiType.java` -> `Effect` class per type | `ApiType` enum + `match` dispatch to handler functions | One function per effect type, compiler enforces exhaustiveness |
| `TriggerType.java` -> `Trigger` class per type | `TriggerMode` enum + matcher functions | Same pattern as effects |
| `GameAction.java` (mutable, stateful) | `GameAction` enum (immutable, data-only) | Actions are data, not behavior. The dispatcher interprets them |
| `Game.java` (mutable game loop) | `dispatch()` pure function | No mutable game object. Each call produces new state |
| `CardProperty` / `CardState` (mutable fields on Card) | `im::HashMap` of modifications keyed by `CardInstanceId` | Structural sharing instead of mutation |
| `StaticAbilityLayer` (enum + visitor) | `Layer` enum + sequential evaluation function | Layers evaluate in order 1-7, each a pure function |
| `CostPart` hierarchy | `Cost` enum with variants (`ManaCost`, `TapCost`, `SacrificeCost`, etc.) | Flat enum, not inheritance |
| `ComputerUtil` + per-card AI logic | `ai::heuristics` module + per-ApiType evaluation functions | AI logic organized by what it evaluates, not by card |

---

## Sources

- [Argentum MTG Engine Architecture](https://wingedsheep.com/building-argentum-a-magic-the-gathering-rules-engine/) -- ECS-like architecture, deterministic engine, state projection pattern
- [Rust Forum: Card Game Rules Engine Architecture](https://users.rust-lang.org/t/architecture-discussion-writing-a-card-game-rules-engine-in-rust/41569) -- Effect-based command pattern, sequential modifier application
- [Tauri v2: Calling Rust from Frontend](https://v2.tauri.app/develop/calling-rust/) -- Commands, channels, state management, async patterns
- [Tauri v2: Calling Frontend from Rust](https://v2.tauri.app/develop/calling-frontend/) -- Event emission, channels for streaming
- [Tauri v2: IPC Concepts](https://v2.tauri.app/concept/inter-process-communication/) -- Message passing architecture, security model
- [im crate: Persistent Data Structures](https://docs.rs/im/latest/im/) -- Structural sharing for immutable collections
- [enum_dispatch crate](https://docs.rs/enum_dispatch/latest/enum_dispatch/) -- ~10x faster than trait object dispatch
- [tsify crate](https://lib.rs/crates/tsify) -- TypeScript type generation from Rust structs
- [serde-wasm-bindgen](https://docs.rs/serde-wasm-bindgen/latest/serde_wasm_bindgen/) -- Native serde integration with WASM
- [Rust Polymorphism: Enums vs Traits](https://www.mattkennedy.io/blog/rust_polymorphism/) -- When to use each pattern
- [tauri-interop crate](https://lib.rs/crates/tauri-interop) -- Generate WASM functions from Tauri commands
