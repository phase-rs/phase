# Phase 26: Polish and Fix Multi-player with Lobby and Embedded Server - Research

**Researched:** 2026-03-10
**Domain:** Multiplayer networking (WebSocket, WebRTC P2P), Tauri sidecar, game lobby UX
**Confidence:** HIGH

## Summary

Phase 26 covers five distinct workstreams: (1) fixing five known WebSocket multiplayer bugs, (2) building a real-time game lobby with server-side protocol extensions, (3) embedding the phase-server as a Tauri sidecar for desktop hosting, (4) implementing P2P WebRTC hosting for browser/PWA users via PeerJS (ported from the Alchemy project at `../alchemy`), and (5) adding in-game multiplayer UX features (concede, emotes, connection status, game over screen, player identity).

The existing codebase is well-structured for extension. The `EngineAdapter` interface, discriminated union protocol patterns (`ClientMessage`/`ServerMessage` with `#[serde(tag = "type", content = "data")]`), and Zustand store architecture provide clear extension points. The Alchemy project's `network/` module provides a battle-tested PeerJS WebRTC implementation that can be directly ported. The five bugs are all straightforward fixes with clear root causes identified in the CONTEXT.md.

**Primary recommendation:** Execute in 4 waves -- (1) bug fixes + player identity, (2) server lobby protocol + frontend lobby UI, (3) P2P WebRTC adapter + Tauri sidecar, (4) in-game multiplayer UX (concede, emotes, timer, game over).

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions
- Fix Bug A: Clear stale `phase-ws-session` from sessionStorage on new game creation -- prevents reconnect to dead sessions
- Fix Bug B: Validate deck selection before navigating to game page for online mode (same guard as WASM path)
- Fix Bug C: Handle `StateUpdate` when no `pendingResolve` exists -- update Zustand store directly for opponent actions
- Fix Bug D: Implement `getAiAction()` on WebSocketAdapter as a no-op/error (AI not used in multiplayer)
- Fix Bug E: Use `your_player` from `GameStarted` message instead of hardcoded `PLAYER_ID = 0`
- Browseable game list + manual code entry in a single lobby view
- Real-time lobby updates via WebSocket (server pushes game list changes)
- Game listing shows: host display name, wait time, game code
- Public/private toggle at host time -- "List in lobby" checkbox
- Optional password protection for games
- Only waiting games shown in lobby; auto-expire stale games
- Show online player count in the lobby
- P2P games do NOT appear in lobby -- code-only
- Player-chosen display name persisted to localStorage
- Stable UUID player ID in localStorage
- Display name editable in Multiplayer settings
- Opponent display name shown during gameplay
- Tauri sidecar: Ship phase-server as Tauri sidecar binary, spawned on "Host Game", auto-stops when game ends, binds to 0.0.0.0 for LAN
- P2P WebRTC: Port Alchemy's implementation. Host-authoritative -- host runs WASM engine, opponent receives state. Uses Alchemy's TURN/signaling setup
- Separate `P2PAdapter` class implementing `EngineAdapter` -- not extending WebSocketAdapter
- Both hosting modes available on Tauri; sidecar is default. Browser/PWA uses P2P only (can join server games via code)
- Host setup screen with: display name, hosting mode, public/private toggle, password, format, per-turn timer
- Per-turn chess-clock timer: configurable time per turn
- Smart server detection priority: (1) Tauri sidecar localhost, (2) last-used server, (3) manual entry
- `CODE@IP:PORT` syntax for join codes
- Toast on connection failure with Retry / Change Server
- Connection status dot (green/yellow/red) throughout multiplayer flow
- Dedicated "Multiplayer" section in settings
- Menu flow: mode-select -> deck-gallery-online -> lobby -> host-setup -> waiting
- Waiting screen shows game code, LAN IP:port, "Listed in lobby" label, cancel button
- Concede button with confirmation dialog
- Quick emotes: pre-set messages (MTGA-style, no free text)
- Simple game over results screen with winner, turns, duration, "Back to Lobby" button

### Claude's Discretion
- Exact emote set and UI placement
- Timer enforcement implementation details (server-side vs client-side)
- Sidecar lifecycle management details (port selection, health check)
- Auto-expire timeout duration for stale lobby games
- P2P signaling relay implementation details
- Connection status dot exact positioning and animation

### Deferred Ideas (OUT OF SCOPE)
- Cross-device player identity / account system
- Friends list / friend invites
- Spectator mode
- Rematch button
- Detailed game statistics
- Turn timer with time bank
</user_constraints>

## Standard Stack

### Core
| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| PeerJS | ^1.5.5 | WebRTC P2P connections via DataChannel | Used in Alchemy, wraps WebRTC with signaling server, proven in this codebase |
| @tauri-apps/plugin-shell | ^2 | Tauri sidecar management | Official Tauri plugin for spawning/managing external binaries |
| zustand | 5.0.11 | Lobby/multiplayer state management | Already used project-wide for stores |
| framer-motion | 12.35.1 | Lobby/waiting screen animations | Already used project-wide |

### Supporting
| Library | Version | Purpose | When to Use |
|---------|---------|---------|-------------|
| uuid (native `crypto.randomUUID()`) | N/A | Player identity UUID generation | Built into all target browsers, no dependency needed |
| axum | 0.8 | WebSocket server with lobby extensions | Already used in phase-server |
| tokio | 1 | Async runtime for server | Already used in phase-server |

### Alternatives Considered
| Instead of | Could Use | Tradeoff |
|------------|-----------|----------|
| PeerJS | Raw WebRTC API | PeerJS handles STUN/TURN/signaling; raw requires building all of that |
| PeerJS | simple-peer | PeerJS already proven in Alchemy; switching adds risk for no benefit |

**Installation:**
```bash
cd client && pnpm add peerjs@^1.5.5
cd client && pnpm add @tauri-apps/plugin-shell@^2
```

Note: `@tauri-apps/plugin-shell` is only needed for Tauri sidecar support. The `peerjs` package is the primary new runtime dependency.

## Architecture Patterns

### Recommended Project Structure
```
client/src/
├── adapter/
│   ├── types.ts              # EngineAdapter interface (existing)
│   ├── wasm-adapter.ts       # Existing
│   ├── ws-adapter.ts         # Bug fixes C, D, E applied here
│   ├── tauri-adapter.ts      # Existing
│   └── p2p-adapter.ts        # NEW: P2PAdapter implementing EngineAdapter
├── network/                   # NEW: Ported from Alchemy
│   ├── connection.ts          # hostRoom(), joinRoom(), PeerJS config
│   ├── peer.ts                # createPeerSession(), PeerSession type
│   └── protocol.ts            # P2P message types
├── stores/
│   ├── gameStore.ts           # Existing (minimal changes)
│   ├── multiplayerStore.ts    # NEW: Lobby state, player identity, connection status
│   └── preferencesStore.ts    # Extend with multiplayer settings
├── components/
│   ├── lobby/                 # NEW: Lobby components
│   │   ├── LobbyView.tsx      # Game list + code entry
│   │   ├── HostSetup.tsx      # Host configuration screen
│   │   ├── WaitingScreen.tsx   # Waiting for opponent
│   │   └── GameListItem.tsx   # Individual game listing
│   ├── multiplayer/           # NEW: In-game multiplayer UI
│   │   ├── EmoteOverlay.tsx   # Emote buttons and display
│   │   ├── ConnectionDot.tsx  # Connection status indicator
│   │   └── ConcedeDialog.tsx  # Concede confirmation
│   └── chrome/
│       └── GameMenu.tsx       # Extend with concede for multiplayer
├── pages/
│   └── MenuPage.tsx           # Extend menu state machine
└── providers/
    └── GameProvider.tsx        # Extend for P2P adapter path

crates/
├── server-core/src/
│   ├── protocol.rs            # Extend ClientMessage/ServerMessage for lobby
│   ├── session.rs             # Extend SessionManager with lobby data
│   ├── lobby.rs               # NEW: Lobby manager (game listing, expiry)
│   └── lib.rs                 # Re-export lobby module
└── phase-server/src/
    └── main.rs                # Extend for lobby WebSocket protocol
```

### Pattern 1: Lobby Protocol Extension
**What:** Extend the existing discriminated union protocol with lobby-specific messages
**When to use:** All server-lobby communication
**Example:**
```rust
// In crates/server-core/src/protocol.rs
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ClientMessage {
    // Existing variants...
    CreateGame { deck: DeckData },
    JoinGame { game_code: String, deck: DeckData },
    Action { action: GameAction },
    Reconnect { game_code: String, player_token: String },

    // NEW lobby variants
    ListGames,
    SubscribeLobby,
    UnsubscribeLobby,
    CreateGameWithSettings {
        deck: DeckData,
        display_name: String,
        public: bool,
        password: Option<String>,
        timer_seconds: Option<u32>,
    },
    JoinGameWithPassword {
        game_code: String,
        deck: DeckData,
        display_name: String,
        password: Option<String>,
    },
    Concede,
    Emote { emote: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ServerMessage {
    // Existing variants...

    // NEW lobby variants
    LobbyUpdate { games: Vec<LobbyGame> },
    LobbyGameAdded { game: LobbyGame },
    LobbyGameRemoved { game_code: String },
    PlayerCount { count: u32 },
    PasswordRequired { game_code: String },
    Emote { from_player: PlayerId, emote: String },
    Conceded { player: PlayerId },
    TimerUpdate { player: PlayerId, remaining_seconds: u32 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LobbyGame {
    pub game_code: String,
    pub host_name: String,
    pub created_at: u64,      // timestamp
    pub has_password: bool,
}
```

### Pattern 2: P2PAdapter (Host-Authoritative)
**What:** A new EngineAdapter that wraps WASM engine for the host and relays state to guest via PeerJS DataChannel
**When to use:** Browser/PWA multiplayer and as option in Tauri
**Key difference from Alchemy:** In Alchemy, both peers run the engine independently (lock-step). In phase.rs, the host is authoritative -- host runs WASM engine, filters state, sends to guest. This prevents cheating and matches the existing server model.
**Example:**
```typescript
// client/src/adapter/p2p-adapter.ts
export class P2PHostAdapter implements EngineAdapter {
  private wasmAdapter: WasmAdapter;
  private session: PeerSession;
  // Host runs WASM locally, sends filtered state to guest
}

export class P2PGuestAdapter implements EngineAdapter {
  private session: PeerSession;
  private gameState: GameState | null = null;
  // Guest receives state from host, sends actions
}
```

### Pattern 3: Multiplayer Store
**What:** Zustand store for lobby/multiplayer state separate from game state
**When to use:** Lobby UI, connection management, player identity
**Example:**
```typescript
// client/src/stores/multiplayerStore.ts
interface MultiplayerState {
  playerId: string;          // UUID from localStorage
  displayName: string;       // from localStorage
  serverAddress: string;     // last-used server
  connectionStatus: 'disconnected' | 'connecting' | 'connected';
  lobbyGames: LobbyGame[];
  playerCount: number;
}
```

### Pattern 4: Menu State Machine Extension
**What:** Extend the existing `MenuView` discriminated union with new lobby states
**When to use:** MenuPage navigation
**Example:**
```typescript
type MenuView =
  | "mode-select"
  | "deck-gallery-ai"
  | "deck-gallery-online"
  | "lobby"           // NEW: browseable game list + code entry
  | "host-setup"      // NEW: host configuration screen
  | "waiting"         // NEW: waiting for opponent (replaces inline GamePage overlay)
  | "join-code";      // Keep for direct code entry (redundant with lobby but backwards-compat)
```

### Anti-Patterns to Avoid
- **Extending WebSocketAdapter for P2P:** CONTEXT explicitly requires a separate `P2PAdapter` class. The protocols are fundamentally different (WS talks to server, P2P talks peer-to-peer). Don't try to abstract both behind a common base class.
- **Shared engine state in P2P:** In the host-authoritative model, only the host runs the engine. The guest adapter must NOT initialize WASM -- it receives state from the host.
- **Hardcoding PLAYER_ID in components:** Bug E requires using `your_player` from the server. Components that use `PLAYER_ID` constant need to read from a dynamic source (store or context).

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| WebRTC signaling | Custom signaling server | PeerJS (uses their cloud signaling) | Signaling is complex, PeerJS handles STUN/TURN/signaling in one package |
| TURN relay | Self-hosted TURN server | Alchemy's metered.ca TURN credentials | TURN servers are expensive and complex to run; use existing free tier |
| UUID generation | Custom UUID function | `crypto.randomUUID()` | Native browser API, cryptographically secure |
| Process management in Tauri | Custom spawning code | `@tauri-apps/plugin-shell` Command.sidecar() | Official Tauri pattern for sidecar management with proper lifecycle |
| Local storage persistence for settings | Custom serialize/deserialize | Zustand `persist` middleware | Already used for preferencesStore, handles serialization and versioning |

**Key insight:** The P2P networking layer has been solved in the Alchemy project. Porting those ~250 lines of proven code is dramatically safer than building from scratch.

## Common Pitfalls

### Pitfall 1: StateUpdate Without pendingResolve (Bug C)
**What goes wrong:** When the opponent submits an action, the server sends `StateUpdate` to both players. But only the acting player has a `pendingResolve` from their `submitAction()` call. The non-acting player's `StateUpdate` is silently dropped.
**Why it happens:** The `handleMessage` switch in `ws-adapter.ts` lines 286-294 only processes `StateUpdate` if `this.pendingResolve` exists. Non-acting player has no pending promise.
**How to avoid:** When `StateUpdate` arrives with no `pendingResolve`, update `this.gameState` directly AND emit a new `WsAdapterEvent` type (e.g., `stateChanged`) so the Zustand store can pick it up. The `gameStore` needs to listen for these unsolicited state updates.
**Warning signs:** Opponent's cards don't appear/change until you take your own action.

### Pitfall 2: Hardcoded PLAYER_ID = 0 (Bug E)
**What goes wrong:** `GamePage.tsx` imports `PLAYER_ID` from constants and uses it for WaitingFor checks. In multiplayer, the joining player is `PlayerId(1)`, so all their prompts (mulligan, targeting, mana) never render.
**Why it happens:** Single-player assumes human is always player 0.
**How to avoid:** Store `your_player` from `GameStarted` message in the multiplayer store or game store. Replace all `PLAYER_ID` references in GamePage with this dynamic value. For WASM/Tauri modes, default to 0.
**Warning signs:** Player 1 sees the board but can't interact with any prompts.

### Pitfall 3: PeerJS Signaling Server Flakiness
**What goes wrong:** PeerJS uses a free signaling server (`0.peerjs.com`) that can be unreliable, slow, or down.
**Why it happens:** Free infrastructure with no SLA.
**How to avoid:** The Alchemy implementation already overrides PeerJS defaults with metered.ca TURN servers. Keep these credentials. Consider adding a connection timeout with user-friendly error message.
**Warning signs:** "Failed to create room" errors, long connection delays.

### Pitfall 4: Tauri Sidecar Binary Naming
**What goes wrong:** Tauri requires sidecar binaries with platform-specific target triple suffixes. Without the correct suffix, the binary won't be found at runtime.
**Why it happens:** Tauri's `externalBin` resolution appends the target triple to the filename.
**How to avoid:** Create a build script that copies the compiled `phase-server` binary to `src-tauri/binaries/phase-server-{target-triple}`. Use `rustc --print host-tuple` to get the correct suffix.
**Warning signs:** "Failed to spawn sidecar" errors on Tauri builds.

### Pitfall 5: Race Condition in Lobby Subscribe/Unsubscribe
**What goes wrong:** If the user navigates away from the lobby while a WebSocket subscription is active, stale lobby updates continue arriving.
**Why it happens:** WebSocket subscriptions are not automatically cleaned up on component unmount.
**How to avoid:** Send `UnsubscribeLobby` on component unmount (useEffect cleanup). Use a dedicated lobby WebSocket connection separate from the game connection, or manage subscription state carefully.
**Warning signs:** Memory leaks, console warnings about updating unmounted components.

### Pitfall 6: P2P State Desynchronization
**What goes wrong:** Host and guest state diverge, causing mismatched board views.
**Why it happens:** In host-authoritative model, if a state update is dropped over DataChannel, the guest falls behind.
**How to avoid:** Always send full filtered state with each action result (matching the server's StateUpdate pattern). Include sequence numbers. On sequence gap, guest can request full state resync.
**Warning signs:** Guest sees different board state than host.

## Code Examples

### Bug A Fix: Clear Stale Session
```typescript
// In MenuPage.tsx or GameProvider.tsx, when starting a new online game:
sessionStorage.removeItem("phase-ws-session");
```

### Bug C Fix: Handle Unsolicited StateUpdate
```typescript
// In ws-adapter.ts handleMessage, modify StateUpdate case:
case "StateUpdate": {
  const data = msg.data as { state: GameState; events: GameEvent[] };
  this.gameState = data.state;
  if (this.pendingResolve) {
    this.pendingResolve(data.events);
    this.pendingResolve = null;
    this.pendingReject = null;
  } else {
    // Opponent action -- emit event for store to pick up
    this.emit({ type: "stateChanged", state: data.state, events: data.events });
  }
  break;
}
```

### Bug D Fix: getAiAction No-Op
```typescript
// In ws-adapter.ts, add the missing method:
getAiAction(_difficulty: string): GameAction | null {
  // AI is not used in multiplayer games
  return null;
}
```

### Bug E Fix: Dynamic Player ID
```typescript
// In ws-adapter.ts, the _playerId is already set from GameStarted.
// Expose it and use it in GamePage instead of PLAYER_ID constant.
// GamePage needs to read playerId from a store/context rather than constant.
```

### P2P Host Adapter Pattern
```typescript
// Based on Alchemy's host-authoritative pattern, adapted for phase.rs
export class P2PHostAdapter implements EngineAdapter {
  private wasm: WasmAdapter;
  private session: PeerSession;
  private playerId: PlayerId = 0;

  async submitAction(action: GameAction): Promise<GameEvent[]> {
    const events = await this.wasm.submitAction(action);
    const state = await this.wasm.getState();
    // Send filtered state to guest
    this.session.send({
      type: 'state_update',
      state: filterStateForPlayer(state, 1),
      events,
    });
    return events;
  }
}
```

### Tauri Sidecar Spawn
```typescript
// Using @tauri-apps/plugin-shell
import { Command } from '@tauri-apps/plugin-shell';

async function spawnSidecar(port: number): Promise<void> {
  const command = Command.sidecar('binaries/phase-server', [], {
    env: { PORT: String(port) },
  });
  const child = await command.spawn();
  // Store child reference for cleanup
}
```

### Multiplayer Store Pattern
```typescript
// client/src/stores/multiplayerStore.ts
import { create } from "zustand";
import { persist } from "zustand/middleware";

interface MultiplayerStore {
  playerId: string;
  displayName: string;
  serverAddress: string;
  setDisplayName: (name: string) => void;
  setServerAddress: (addr: string) => void;
}

export const useMultiplayerStore = create<MultiplayerStore>()(
  persist(
    (set) => ({
      playerId: crypto.randomUUID(),
      displayName: "",
      serverAddress: "ws://localhost:8080/ws",
      setDisplayName: (name) => set({ displayName: name }),
      setServerAddress: (addr) => set({ serverAddress: addr }),
    }),
    { name: "phase-multiplayer" },
  ),
);
```

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| Hardcoded PLAYER_ID = 0 | Dynamic player ID from server | This phase | Multiplayer player 1 can actually play |
| Host/Join flows in GamePage overlays | Dedicated lobby page with real-time game list | This phase | Much better UX for finding games |
| Manual server address configuration | Smart detection (sidecar -> last-used -> manual) | This phase | Zero-config for desktop users |
| Server-only multiplayer | P2P + Server options | This phase | Browser/PWA users can host games |

**Deprecated/outdated:**
- `PLAYER_ID` constant usage in multiplayer context -- must be replaced with dynamic value
- `online-host-join` menu view -- replaced by lobby view

## Open Questions

1. **State Filter in P2P Host**
   - What we know: Server uses `filter_state_for_player()` in Rust. P2P host runs WASM in browser.
   - What's unclear: The WASM bridge doesn't expose `filter_state_for_player`. The host P2P adapter needs to filter state before sending to guest.
   - Recommendation: Implement state filtering in TypeScript on the P2P host side. The filtering logic is simple (hide opponent hand, hide libraries) and can mirror the Rust implementation in ~30 lines of TS.

2. **Lobby WebSocket vs Game WebSocket**
   - What we know: Currently one WebSocket connection per game session. Lobby needs a persistent connection for game list updates.
   - What's unclear: Whether to reuse the same WebSocket for lobby and game, or use separate connections.
   - Recommendation: Use the same WebSocket connection. Add `SubscribeLobby`/`UnsubscribeLobby` messages. When a user joins/creates a game, the connection transitions from lobby mode to game mode. This avoids managing multiple connections.

3. **Timer Enforcement Location**
   - What we know: Per-turn chess clock is desired. Server validates actions.
   - What's unclear: Whether timer enforcement should be server-side (authoritative) or client-side (trusting).
   - Recommendation: Server-side enforcement for server-hosted games (add timeout to `handle_action`). For P2P, host-side enforcement since host is authoritative. Send `TimerUpdate` messages periodically so clients can display countdown.

4. **Sidecar Port Selection**
   - What we know: Sidecar needs to bind to a port. Port 8080 is the default.
   - What's unclear: How to handle port conflicts if 8080 is in use.
   - Recommendation: Try a range of ports (8080-8089). Check with a health endpoint before using. Pass selected port as environment variable to sidecar.

## Validation Architecture

### Test Framework
| Property | Value |
|----------|-------|
| Framework | Vitest (frontend), cargo test (Rust) |
| Config file | `client/vitest.config.ts` (implied by pnpm test), inline `#[cfg(test)]` (Rust) |
| Quick run command | `cd client && pnpm test -- --run --reporter=verbose` |
| Full suite command | `cd client && pnpm test -- --run && cargo test --all` |

### Phase Requirements -> Test Map
| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| BUG-A | Clear stale session on new game | unit | `cd client && pnpm test -- --run -t "stale session"` | Wave 0 |
| BUG-B | Deck validation before online game | unit | `cd client && pnpm test -- --run -t "deck validation"` | Wave 0 |
| BUG-C | Handle StateUpdate without pendingResolve | unit | `cd client && pnpm test -- --run -t "unsolicited state"` | Wave 0 |
| BUG-D | getAiAction returns null for WS adapter | unit | `cd client && pnpm test -- --run -t "getAiAction"` | Wave 0 |
| BUG-E | Dynamic player ID from GameStarted | unit | `cd client && pnpm test -- --run -t "player id"` | Wave 0 |
| LOBBY-SRV | Server lobby protocol (create/list/join with settings) | unit | `cargo test -p server-core -- lobby` | Wave 0 |
| LOBBY-UI | Lobby renders game list and join code input | unit | `cd client && pnpm test -- --run -t "LobbyView"` | Wave 0 |
| P2P | PeerSession send/receive/disconnect | unit | `cd client && pnpm test -- --run -t "PeerSession"` | Wave 0 |
| SIDECAR | Sidecar spawn and health check | manual-only | Manual: requires Tauri desktop build | N/A |
| EMOTE | Emote message roundtrip | unit | `cargo test -p server-core -- emote` | Wave 0 |

### Sampling Rate
- **Per task commit:** `cd client && pnpm test -- --run && cargo test -p server-core`
- **Per wave merge:** `cd client && pnpm test -- --run && cargo test --all`
- **Phase gate:** Full suite green before `/gsd:verify-work`

### Wave 0 Gaps
- [ ] `client/src/adapter/__tests__/ws-adapter.test.ts` -- extend with Bug C/D/E test cases
- [ ] `client/src/network/__tests__/peer.test.ts` -- port from Alchemy + adapt
- [ ] `client/src/stores/__tests__/multiplayerStore.test.ts` -- new store tests
- [ ] `crates/server-core/src/lobby.rs` -- lobby manager with inline `#[cfg(test)]` module
- [ ] Protocol roundtrip tests for new `ClientMessage`/`ServerMessage` variants

## Sources

### Primary (HIGH confidence)
- Codebase inspection: `client/src/adapter/ws-adapter.ts` -- identified all 5 bugs
- Codebase inspection: `client/src/adapter/types.ts` -- EngineAdapter interface
- Codebase inspection: `crates/server-core/src/protocol.rs` -- existing protocol
- Codebase inspection: `crates/server-core/src/session.rs` -- SessionManager with `open_games()`
- Codebase inspection: `crates/phase-server/src/main.rs` -- server architecture
- Codebase inspection: `../alchemy/src/network/` -- PeerJS WebRTC implementation
- Codebase inspection: `client/src-tauri/tauri.conf.json` -- Tauri v2 configuration
- Codebase inspection: `client/src/pages/MenuPage.tsx` -- menu state machine
- Codebase inspection: `client/src/pages/GamePage.tsx` -- PLAYER_ID usage pattern

### Secondary (MEDIUM confidence)
- [Tauri v2 Sidecar Documentation](https://v2.tauri.app/develop/sidecar/) -- externalBin config, permissions, shell plugin API
- [PeerJS npm](https://www.npmjs.com/package/peerjs) -- v1.5.5, latest stable

### Tertiary (LOW confidence)
- Timer enforcement approach -- no reference implementation in codebase; recommendation based on general game server architecture

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH -- all libraries already in use or proven in sibling project
- Architecture: HIGH -- extends existing patterns (EngineAdapter, protocol unions, Zustand stores)
- Bug fixes: HIGH -- root causes identified directly from code inspection
- P2P adapter: HIGH -- Alchemy implementation is a direct port target
- Tauri sidecar: MEDIUM -- configuration pattern clear from docs, but untested in this project
- Lobby protocol: HIGH -- extends existing discriminated union protocol pattern
- Timer enforcement: LOW -- design decision, multiple valid approaches

**Research date:** 2026-03-10
**Valid until:** 2026-04-10 (stable domain, 30 days)
