# Phase 12: Multiplayer Wiring & Final Cleanup - Research

**Researched:** 2026-03-08
**Domain:** Server-side deck resolution, WebSocket reconnection, endpoint cleanup, tech debt closure
**Confidence:** HIGH

## Summary

Phase 12 closes the remaining integration gaps between the multiplayer server and client. The core problem is straightforward: the server receives deck card names as strings (`DeckData.main_deck: Vec<String>`) but never resolves them to `CardFace` objects, so multiplayer games start with empty libraries. The fix requires loading `CardDatabase` at server startup and resolving card names before calling `start_game()`.

The second gap is that `WebSocketAdapter.tryReconnect()` is fully implemented but never called. The client needs to invoke it on WebSocket close events (with retry logic) and on page reload (checking `sessionStorage`). The remaining items are trivial: removing an orphaned HTTP route and verifying two already-completed tech debt items.

**Primary recommendation:** Wire `CardDatabase` into the server startup path, add a `resolve_deck()` function in `server-core` that converts `DeckData` name strings to `DeckPayload` (reusing `engine::game::DeckEntry`/`DeckPayload`), then hook reconnect triggers into the WsAdapter's `onclose` handler with exponential backoff retry.

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions
- Load `CardDatabase` at server startup in `main()`, pass as shared state (`Arc<CardDatabase>`)
- Card files path via `FORGE_CARDS_DIR` environment variable; error on startup if missing
- Reject game creation/join if any card name in `DeckData.main_deck` can't be resolved via `CardDatabase`
- Deck-to-GameObjects conversion function lives in the `engine` crate (not server-core) so both WASM and server paths can reuse it
- Reconnect on both WebSocket `onclose` event and page reload (check `sessionStorage` on mount)
- 3 retries with exponential backoff (1s, 2s, 4s)
- Non-blocking overlay banner at top of GamePage: "Reconnecting... (attempt N/3)" -- game board visible but interactions disabled
- After all retries fail: banner changes to "Connection lost" with a "Retry" button and a "Return to Menu" link
- Remove the `/games` HTTP route from `forge-server/src/main.rs`
- Keep `open_games()` method on `SessionManager` -- lobby UI is planned soon
- Remove the `list_games` handler function

### Claude's Discretion
- Exact retry timing constants (1s/2s/4s suggested but flexible)
- Banner styling and animation
- Error message wording for deck resolution failures
- Whether to log unresolvable card names before rejecting

### Deferred Ideas (OUT OF SCOPE)
- Lobby UI with game list and join buttons -- future phase (user confirmed this is planned)
</user_constraints>

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|-----------------|
| MP-01 | WebSocket server for authoritative game state | Deck resolution gap: server receives `DeckData` (string names) but never resolves to `CardFace` via `CardDatabase`. `load_deck_into_state()` in engine crate already handles the `DeckPayload` -> `GameObject` conversion. Need a `resolve_deck()` bridge function. |
| MP-03 | Action synchronization (send actions, not full state) | Reconnect gap: `tryReconnect()` exists on `WebSocketAdapter` but is never called. Need `onclose` handler with retry logic and `sessionStorage` check on mount. |
</phase_requirements>

## Standard Stack

### Core (Already in Project)
| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| engine (crate) | workspace | `CardDatabase`, `DeckPayload`, `load_deck_into_state`, `start_game` | All deck resolution logic already exists |
| server-core (crate) | workspace | `SessionManager`, `DeckData`, `ReconnectManager` | Session management layer |
| forge-server (crate) | workspace | Axum WebSocket server | Entry point with shared state pattern |
| axum | 0.8 | HTTP/WebSocket server | Already used for /ws, /health, /games routes |

### No New Dependencies Required
This phase wires existing code together. No new crates or npm packages needed. The `engine` crate already has `CardDatabase` (uses `walkdir` internally). The `forge-server` already depends on `engine`.

## Architecture Patterns

### Pattern 1: Server Deck Resolution Flow

**What:** Convert `DeckData` (card name strings) to `DeckPayload` (CardFace objects) using `CardDatabase` at game creation/join time.

**Current flow (broken):**
```
Client sends DeckData { main_deck: ["Forest", "Lightning Bolt", ...] }
  -> SessionManager.join_game() stores DeckData
  -> start_game(&mut state) called with empty libraries
  -> Game starts with no cards
```

**Target flow:**
```
Server startup: CardDatabase::load(FORGE_CARDS_DIR) -> Arc<CardDatabase>
Client sends DeckData { main_deck: ["Forest", "Lightning Bolt", ...] }
  -> resolve_deck(&db, &deck_data) -> Result<Vec<DeckEntry>, String>
     - For each name: db.get_face_by_name(name) -> CardFace
     - Group by name, count occurrences -> Vec<DeckEntry>
     - If any name unresolvable: return Err with list of missing cards
  -> load_deck_into_state(&mut state, &payload)
  -> start_game(&mut state)
```

**Key types already available:**
```rust
// engine::database::CardDatabase
pub fn get_face_by_name(&self, name: &str) -> Option<&CardFace>

// engine::game::DeckPayload (used by WASM path)
pub struct DeckPayload {
    pub player_deck: Vec<DeckEntry>,
    pub opponent_deck: Vec<DeckEntry>,
}

pub struct DeckEntry {
    pub card: CardFace,
    pub count: u32,
}

// server_core::protocol::DeckData (sent by client)
pub struct DeckData {
    pub main_deck: Vec<String>,  // Card names
    pub sideboard: Vec<String>,
}
```

### Pattern 2: Shared State Threading

**What:** Pass `Arc<CardDatabase>` alongside existing `Arc<Mutex<SessionManager>>`.

**Current shared state pattern in main.rs:**
```rust
type SharedState = Arc<Mutex<SessionManager>>;
// Router uses .with_state((state, connections))
```

**Target:** Add `Arc<CardDatabase>` to the state tuple. `CardDatabase` is read-only after load, so it needs `Arc` but not `Mutex`.

```rust
type SharedDb = Arc<CardDatabase>;
// Router uses .with_state((state, connections, db))
```

This follows the existing pattern and requires updating handler signatures to extract the new state component.

### Pattern 3: Client Reconnect with Exponential Backoff

**What:** Wire `tryReconnect()` calls into WebSocket lifecycle.

**Current state:**
- `WebSocketAdapter.tryReconnect()` fully implemented -- reads sessionStorage, opens new WebSocket, sends Reconnect message
- `sessionStorage` key `forge-ws-session` already persisted on game creation
- Server-side `handle_reconnect()` fully implemented with grace period

**Missing pieces:**
1. `ws.onclose` handler during gameplay (current onclose only handles pre-game-start case)
2. Retry loop with backoff (1s, 2s, 4s)
3. UI state for reconnection status
4. Page-reload detection (check sessionStorage on GamePage mount)

**Approach:** Add retry logic as a method on `WebSocketAdapter` that wraps `tryReconnect()` with backoff. Emit new events for reconnection state that GamePage can render as a banner.

### Pattern 4: Reconnect Event Extension

**Current WsAdapterEvent types:**
```typescript
export type WsAdapterEvent =
  | { type: "gameCreated"; gameCode: string }
  | { type: "waitingForOpponent" }
  | { type: "opponentDisconnected"; graceSeconds: number }
  | { type: "opponentReconnected" }
  | { type: "gameOver"; winner: PlayerId | null; reason: string }
  | { type: "error"; message: string };
```

**New events needed:**
```typescript
  | { type: "reconnecting"; attempt: number; maxAttempts: number }
  | { type: "reconnected" }
  | { type: "reconnectFailed" }
```

### Anti-Patterns to Avoid
- **Don't create a new DeckPayload type in server-core:** Reuse `engine::game::DeckPayload` and `DeckEntry` directly.
- **Don't resolve decks lazily:** Resolve at create/join time so errors surface immediately.
- **Don't block the game board during reconnection:** User decided non-blocking banner (board visible, interactions disabled).

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| Card name -> CardFace resolution | Custom card lookup | `CardDatabase::get_face_by_name()` | Already handles case-insensitive lookup, multi-face cards |
| Deck -> GameObjects conversion | Custom object creation | `load_deck_into_state()` from engine crate | Handles keywords, abilities, triggers, shuffling |
| Session data persistence | Custom storage | `sessionStorage` via existing `WS_STORAGE_KEY` pattern | Already implemented in `persistSession()` |
| Reconnect protocol | Custom handshake | Existing `ClientMessage::Reconnect` + `ServerMessage::GameStarted` | Server already handles reconnect and sends filtered state |

## Common Pitfalls

### Pitfall 1: CardDatabase Requires Filesystem
**What goes wrong:** `CardDatabase::load()` reads `.txt` files from disk via `walkdir`. This works on the server (native Rust) but NOT in WASM.
**Why it matters:** The decision to put deck resolution in the engine crate is about the conversion function (`create_object_from_card_face`, `load_deck_into_state`), NOT `CardDatabase` itself. The resolve function that maps names to faces will live in server-core since it needs `CardDatabase`.
**How to avoid:** The `resolve_deck()` function takes `&CardDatabase` as a parameter. The engine crate provides `DeckPayload`/`DeckEntry`/`load_deck_into_state`. Server-core provides the name-to-face resolution bridge.

### Pitfall 2: Duplicate Card Name Counting
**What goes wrong:** A deck with 4x "Forest" should create one `DeckEntry { card: forest_face, count: 4 }`, not four entries with `count: 1`.
**Why it matters:** `load_deck_into_state` iterates `0..entry.count` for each entry. Either approach works functionally, but grouping by name matches the `DeckPayload` convention used by the WASM path.
**How to avoid:** Group `DeckData.main_deck` names, count occurrences, resolve each unique name once.

### Pitfall 3: WebSocket onclose Fires During Normal Dispose
**What goes wrong:** When the user navigates away from GamePage, `dispose()` calls `ws.close()`, which triggers `onclose`. If reconnect logic is on `onclose`, it will try to reconnect when the user intentionally left.
**Why it matters:** Reconnect attempt after intentional navigation wastes resources and may cause errors.
**How to avoid:** Set a `disposed` flag in `dispose()` that the `onclose` handler checks before initiating reconnect.

### Pitfall 4: Reconnect Race with Component Unmount
**What goes wrong:** Retry timer fires after GamePage unmounts, trying to update state on an unmounted component.
**Why it matters:** React warnings, potential memory leaks.
**How to avoid:** Clear retry timers in the cleanup function of the GamePage useEffect. The `dispose()` method should cancel pending retries.

### Pitfall 5: Server Startup Fails Without Card Directory
**What goes wrong:** Server panics at startup if `FORGE_CARDS_DIR` env var is missing or points to nonexistent directory.
**Why it matters:** This is the intended behavior per user decision (error on startup if missing).
**How to avoid:** Use `expect()` with a clear error message: `"FORGE_CARDS_DIR environment variable must be set to Forge card files directory"`.

## Code Examples

### Deck Resolution Function (server-core)
```rust
// server-core/src/deck_resolve.rs
use engine::database::CardDatabase;
use engine::game::{DeckEntry, DeckPayload};
use crate::protocol::DeckData;
use std::collections::HashMap;

pub fn resolve_deck(
    db: &CardDatabase,
    deck: &DeckData,
) -> Result<Vec<DeckEntry>, String> {
    let mut counts: HashMap<String, u32> = HashMap::new();
    for name in &deck.main_deck {
        *counts.entry(name.clone()).or_default() += 1;
    }

    let mut entries = Vec::new();
    let mut missing = Vec::new();
    for (name, count) in counts {
        match db.get_face_by_name(&name) {
            Some(face) => entries.push(DeckEntry {
                card: face.clone(),
                count,
            }),
            None => missing.push(name),
        }
    }

    if !missing.is_empty() {
        return Err(format!("Unresolvable cards: {}", missing.join(", ")));
    }

    Ok(entries)
}
```

### Server Startup with CardDatabase
```rust
// forge-server/src/main.rs (modified startup)
let cards_dir = std::env::var("FORGE_CARDS_DIR")
    .expect("FORGE_CARDS_DIR environment variable must be set");
let card_db = CardDatabase::load(std::path::Path::new(&cards_dir))
    .expect("Failed to load card database");
println!("Loaded {} cards", card_db.card_count());
let db: Arc<CardDatabase> = Arc::new(card_db);
```

### Reconnect Retry Logic (ws-adapter.ts)
```typescript
// Inside WebSocketAdapter
private reconnectAttempt = 0;
private readonly maxReconnectAttempts = 3;
private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
private disposed = false;

private attemptReconnect(): void {
    if (this.disposed) return;
    if (this.reconnectAttempt >= this.maxReconnectAttempts) {
        this.emit({ type: "reconnectFailed" });
        return;
    }
    this.reconnectAttempt++;
    const delay = Math.pow(2, this.reconnectAttempt - 1) * 1000; // 1s, 2s, 4s
    this.emit({
        type: "reconnecting",
        attempt: this.reconnectAttempt,
        maxAttempts: this.maxReconnectAttempts,
    });
    this.reconnectTimer = setTimeout(() => {
        this.tryReconnect();
    }, delay);
}
```

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| Empty libraries in multiplayer | Resolve DeckData via CardDatabase | Phase 12 | Games start with actual cards |
| tryReconnect() never called | Auto-reconnect on disconnect + reload | Phase 12 | Resilient multiplayer sessions |

**Already completed (verify only):**
- PermanentCard.tsx already imports and uses `COMBAT_TILT_DEGREES` from `constants/ui.ts` (line 5, line 108)
- TD-01 through TD-06 already in REQUIREMENTS.md traceability table (lines 236-242)

## Open Questions

1. **SessionManager API change for CardDatabase**
   - What we know: `create_game()` and `join_game()` currently take `DeckData` (string names). They need `CardDatabase` access for resolution.
   - Options: (a) Pass `&CardDatabase` to `create_game`/`join_game`, (b) Resolve in `main.rs` before calling session methods, (c) Store `Arc<CardDatabase>` on `SessionManager`.
   - Recommendation: Option (b) -- resolve in the handler before calling session methods. Keeps SessionManager simple and avoids changing its constructor. The resolved `DeckPayload` is passed to `load_deck_into_state` directly.

2. **SessionManager.join_game needs deck loading integration**
   - What we know: Currently `join_game` calls `start_game(&mut session.state)` with no deck objects. After resolution, it needs to call `load_deck_into_state` with both players' decks before `start_game`.
   - Recommendation: Modify `join_game` to accept resolved `DeckPayload` (or split into resolve-then-join). The join handler in main.rs resolves both decks then passes the full payload.

## Validation Architecture

### Test Framework
| Property | Value |
|----------|-------|
| Framework | Rust: cargo test, Frontend: vitest 3.x |
| Config file | `client/vitest.config.ts`, Cargo workspace |
| Quick run command | `cargo test -p server-core` / `cd client && pnpm test -- --run` |
| Full suite command | `cargo test --all` / `cd client && pnpm test -- --run` |

### Phase Requirements -> Test Map
| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| MP-01 | Deck names resolved to CardFace objects | unit | `cargo test -p server-core -- resolve_deck` | No -- Wave 0 |
| MP-01 | Reject unresolvable card names | unit | `cargo test -p server-core -- resolve_deck_missing` | No -- Wave 0 |
| MP-01 | load_deck_into_state populates libraries | unit | `cargo test -p engine -- load_deck` | Yes (existing) |
| MP-03 | Reconnect on WebSocket close triggers retry | unit | `cd client && pnpm test -- --run ws-adapter` | No -- Wave 0 |
| MP-03 | Reconnect on page reload checks sessionStorage | unit | `cd client && pnpm test -- --run ws-adapter` | No -- Wave 0 |
| MP-03 | Retry exhaustion emits reconnectFailed | unit | `cd client && pnpm test -- --run ws-adapter` | No -- Wave 0 |

### Sampling Rate
- **Per task commit:** `cargo test -p server-core -p forge-server && cd client && pnpm test -- --run`
- **Per wave merge:** `cargo test --all && cd client && pnpm test -- --run`
- **Phase gate:** Full suite green before `/gsd:verify-work`

### Wave 0 Gaps
- [ ] `crates/server-core/src/deck_resolve.rs` -- unit tests for resolve_deck function
- [ ] `client/src/adapter/__tests__/ws-adapter.test.ts` -- reconnect behavior tests (may need mock WebSocket)

## Sources

### Primary (HIGH confidence)
- Direct code inspection of all integration points:
  - `crates/engine/src/database/card_db.rs` -- CardDatabase API (get_face_by_name, load)
  - `crates/engine/src/game/deck_loading.rs` -- DeckPayload, DeckEntry, load_deck_into_state, create_object_from_card_face
  - `crates/server-core/src/session.rs` -- SessionManager.join_game (line 105-107: "In a real implementation, decks would be loaded")
  - `crates/server-core/src/protocol.rs` -- DeckData definition
  - `crates/forge-server/src/main.rs` -- Server startup, /games route, shared state pattern
  - `client/src/adapter/ws-adapter.ts` -- WebSocketAdapter.tryReconnect(), persistSession(), event emitter
  - `client/src/pages/GamePage.tsx` -- Online mode setup, WsAdapterEvent handling
  - `client/src/components/board/PermanentCard.tsx` -- Already uses COMBAT_TILT_DEGREES (line 5)
  - `.planning/REQUIREMENTS.md` -- TD-01 through TD-06 already in traceability table (lines 236-242)

### Secondary (HIGH confidence)
- `.planning/v1.0-MILESTONE-AUDIT.md` -- Gap evidence for MP-01, MP-03, orphaned /games endpoint

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH - All code inspected, no new dependencies
- Architecture: HIGH - Wiring existing implementations, clear integration points
- Pitfalls: HIGH - Identified from direct code analysis of lifecycle and state management
- Validation: MEDIUM - WebSocket adapter tests may need mock infrastructure

**Research date:** 2026-03-08
**Valid until:** 2026-04-08 (stable -- internal wiring, no external dependency changes)
