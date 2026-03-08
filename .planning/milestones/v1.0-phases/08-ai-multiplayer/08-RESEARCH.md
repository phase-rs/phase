# Phase 8: AI & Multiplayer - Research

**Researched:** 2026-03-08
**Domain:** Game AI (tree search + heuristics), WebSocket multiplayer, card coverage analysis
**Confidence:** HIGH

## Summary

Phase 8 combines three distinct workstreams: AI opponent, multiplayer networking, and card coverage gap filling. The AI system follows a proven hybrid architecture -- alpha-beta search with Forge-ported domain heuristics -- already validated in the Alchemy project. The multiplayer server uses an authoritative architecture with Axum's native WebSocket support in Rust, filtering hidden information server-side. Card coverage uses automated parsing analysis to identify and prioritize missing handler implementations.

The engine's existing `apply(state, action) -> ActionResult` pure function is ideal for AI game tree search (clone state, try action, evaluate). The `EngineAdapter` interface cleanly supports a new `WebSocketAdapter`. The existing `CardCoverageDashboard` provides a foundation to enhance with per-card Standard analysis. All AI code runs as pure Rust -- same source compiles to native (Tauri, server) and WASM (PWA).

**Primary recommendation:** Build AI as a `forge-ai` crate with `get_legal_actions()` as the foundational primitive, alpha-beta search with iterative deepening, and Forge-ported evaluation heuristics. Build multiplayer as `server-core` library + `forge-server` binary using Axum WebSocket. Tackle card coverage gap-filling by frequency analysis of Standard-legal cards.

<user_constraints>

## User Constraints (from CONTEXT.md)

### Locked Decisions
- **AI approach**: Hybrid game tree search (Alchemy pattern) + selectively ported Forge combat/evaluation heuristics. All AI code pure Rust, compiles to native and WASM.
- **AI difficulty**: 5 levels (Very Easy through Very Hard), scaling via temperature, search depth/nodes, lookahead toggles. Same code native and WASM with tighter WASM budgets.
- **AI combat**: Port Forge's AiAttackController and AiBlockController logic directly.
- **Multiplayer server**: Dedicated Rust WebSocket server, authoritative, hidden info server-side. New WebSocketAdapter implements EngineAdapter. Architecture: server-core library crate, forge-server binary, Tauri host mode imports server-core.
- **Matchmaking**: Direct connect (game code) for v1, lobby browser as second mode.
- **Reconnection**: Grace period (2-5 min), full filtered GameState snapshot on reconnect, timeout = forfeit.
- **Card coverage**: Automated parsing test, per-card coverage report, priority-by-frequency gap filling, enhanced dashboard.
- **AI integration with UI**: Simulated thinking delay (0.5-2s), full animations for AI actions, main menu with mode selection, AI hand hidden by default with debug toggle.

### Claude's Discretion
- Exact tree search algorithm (minimax vs MCTS vs alpha-beta)
- AI search budget defaults per platform (native vs WASM node/depth limits)
- Specific Forge heuristic functions to port vs. skip
- WebSocket protocol message format details
- Server config (port, max games, grace period duration)
- Lobby browser UI design
- AI thinking delay exact timing and variance
- Main menu visual design

### Deferred Ideas (OUT OF SCOPE)
None -- discussion stayed within phase scope.

</user_constraints>

<phase_requirements>

## Phase Requirements

| ID | Description | Research Support |
|----|-------------|-----------------|
| AI-01 | Legal action enumeration from any game state | `get_legal_actions()` function built by inspecting WaitingFor variant + game state; foundation for AI and server validation |
| AI-02 | Board evaluation heuristic | Alchemy's weighted EvalWeights pattern + Forge's CreatureEvaluator (325 LOC) ported to Rust with MTG-specific keyword values |
| AI-03 | Per-card ability decision logic (Forge-level) | Forge's AiPlayDecision SVars on card definitions + ability-type-specific heuristics (when to cast removal, when to use combat tricks) |
| AI-04 | Game tree search (leveraging Rust native performance) | Alpha-beta pruning with iterative deepening; Alchemy's proven search architecture ported to Rust with larger budgets |
| AI-05 | Multiple difficulty levels | 5-level config system (Alchemy pattern): temperature + search depth + lookahead toggles |
| MP-01 | WebSocket server for authoritative game state | Axum + tokio WebSocket server; server-core library crate with session management |
| MP-02 | Hidden information handling | Server-side GameState filtering: strip opponent hand contents, library order before sending |
| MP-03 | Action synchronization | Client sends GameAction, server validates via get_legal_actions + apply(), broadcasts filtered results |
| MP-04 | Reconnection support | Grace period with session preservation, full filtered snapshot on reconnect |
| PLAT-05 | Standard format card coverage (60-70%+) | Automated gap analysis of Standard-legal .txt files, frequency-prioritized handler implementation |

</phase_requirements>

## Standard Stack

### Core (Rust -- AI & Server)

| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| axum | 0.8 | HTTP + WebSocket server framework | Built-in WebSocket support, native tokio integration, dominant Rust web framework |
| tokio | 1.x | Async runtime for server | Standard Rust async runtime, required by axum |
| serde_json | 1.x | WebSocket message serialization | Already used in engine, JSON protocol for debugging ease |
| tower-http | 0.6 | CORS middleware for dev | Pairs with axum for HTTP middleware |

### Core (Rust -- Engine Extensions)

| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| engine (workspace) | 0.1.0 | Game state, apply(), types | Existing crate -- AI and server depend on it |

### Supporting (Client)

| Library | Version | Purpose | When to Use |
|---------|---------|---------|-------------|
| react-router | 7.x | Route for main menu, game setup | Already in use |
| zustand | existing | Game store, AI controller integration | Already in use |

### Alternatives Considered

| Instead of | Could Use | Tradeoff |
|------------|-----------|----------|
| Axum WebSocket | tokio-tungstenite directly | More boilerplate, no HTTP routing for lobby/health endpoints |
| JSON protocol | MessagePack/bincode | JSON is human-readable for debugging; perf not a bottleneck for turn-based game |
| Alpha-beta | MCTS | Alpha-beta is better for deterministic evaluation with good heuristics; MCTS better for games with high branching/stochastic elements. MTG has hidden info but AI simulates from known state, so alpha-beta is appropriate |

**Installation (new crate dependencies):**
```toml
# crates/forge-ai/Cargo.toml
[dependencies]
engine = { path = "../engine" }
serde = { workspace = true }
serde_json = "1"

# crates/server-core/Cargo.toml
[dependencies]
engine = { path = "../engine" }
forge-ai = { path = "../forge-ai" }  # optional, for AI-hosted games
serde = { workspace = true }
serde_json = "1"
tokio = { version = "1", features = ["full"] }

# crates/forge-server/Cargo.toml
[dependencies]
server-core = { path = "../server-core" }
engine = { path = "../engine" }
axum = { version = "0.8", features = ["ws"] }
tokio = { version = "1", features = ["full"] }
tower-http = { version = "0.6", features = ["cors"] }
serde = { workspace = true }
serde_json = "1"
```

## Architecture Patterns

### Recommended Crate Structure
```
crates/
├── engine/              # Existing -- game state, apply(), types
├── engine-wasm/         # Existing -- WASM bindings
├── forge-ai/            # NEW -- AI logic (pure Rust, no platform deps)
│   └── src/
│       ├── lib.rs           # Public API: choose_action(), get_legal_actions()
│       ├── legal_actions.rs # Legal action enumeration from WaitingFor + state
│       ├── eval.rs          # Board evaluation with weighted heuristics
│       ├── combat_ai.rs     # Forge-ported attack/block controllers
│       ├── search.rs        # Alpha-beta with iterative deepening
│       ├── config.rs        # Difficulty presets, search budgets
│       └── card_hints.rs    # Per-card AI hints from SVar/AiPlayDecision
├── server-core/         # NEW -- multiplayer session/protocol library
│   └── src/
│       ├── lib.rs           # Public API
│       ├── session.rs       # Game session management
│       ├── protocol.rs      # WebSocket message types
│       ├── filter.rs        # Hidden information filtering
│       └── reconnect.rs     # Reconnection with grace period
└── forge-server/        # NEW -- standalone WebSocket server binary
    └── src/
        └── main.rs          # Axum server with WS endpoints
```

### Pattern 1: Legal Action Enumeration
**What:** `get_legal_actions(state: &GameState) -> Vec<GameAction>` inspects `state.waiting_for` and generates all valid actions.
**When to use:** Called by AI for decision-making, by server for action validation.
**Example:**
```rust
// crates/forge-ai/src/legal_actions.rs
pub fn get_legal_actions(state: &GameState) -> Vec<GameAction> {
    match &state.waiting_for {
        WaitingFor::Priority { player } => {
            let mut actions = vec![GameAction::PassPriority];
            // Add castable spells from hand
            for &obj_id in &state.players[player.0 as usize].hand {
                if let Some(obj) = state.objects.get(&obj_id) {
                    if can_cast(state, obj, *player) {
                        actions.push(GameAction::CastSpell {
                            card_id: obj.card_id,
                            targets: vec![], // resolved later
                        });
                    }
                }
            }
            // Add playable lands
            // Add activatable abilities
            actions
        }
        WaitingFor::DeclareAttackers { player } => {
            // Enumerate all valid attacker subsets
            enumerate_attacker_combinations(state, *player)
        }
        WaitingFor::DeclareBlockers { player } => {
            enumerate_blocker_assignments(state, *player)
        }
        // ... other WaitingFor variants
        _ => vec![]
    }
}
```

### Pattern 2: Alpha-Beta Search with Iterative Deepening
**What:** Minimax with alpha-beta pruning, iterating depth 1..max_depth, with budget cap on total nodes evaluated.
**When to use:** Medium/Hard/Very Hard difficulty when tree search is enabled.
**Example (matching Alchemy's proven architecture):**
```rust
// crates/forge-ai/src/search.rs
pub fn choose_action_by_search(
    state: &GameState,
    ai_player: PlayerId,
    config: &AiConfig,
    rng: &mut impl Rng,
) -> Option<GameAction> {
    let legal = get_legal_actions(state);
    if legal.len() <= 1 { return legal.into_iter().next(); }

    let mut budget = SearchBudget::new(config.search.max_nodes);
    let mut best_scores: Vec<ScoredAction> = Vec::new();

    // Iterative deepening
    for depth in 1..=config.search.max_depth {
        if budget.exhausted() { break; }
        let mut iteration_scores = Vec::new();
        for action in &legal {
            if budget.exhausted() { break; }
            let mut sim_state = state.clone();
            if apply(&mut sim_state, action.clone()).is_err() { continue; }
            let score = search_value(&sim_state, ai_player, depth - 1,
                f64::NEG_INFINITY, f64::INFINITY, config, &mut budget);
            iteration_scores.push(ScoredAction { action: action.clone(), score });
        }
        if !iteration_scores.is_empty() {
            best_scores = iteration_scores;
        }
    }

    softmax_select(&best_scores, config.temperature, rng)
}
```

### Pattern 3: Authoritative Server with Hidden Info Filtering
**What:** Server holds true GameState, validates actions, sends filtered views to each player.
**When to use:** All multiplayer games.
**Example:**
```rust
// crates/server-core/src/filter.rs
pub fn filter_state_for_player(state: &GameState, player: PlayerId) -> GameState {
    let mut filtered = state.clone();
    let opponent = PlayerId(1 - player.0);

    // Hide opponent's hand contents (replace with face-down placeholders)
    if let Some(opp) = filtered.players.iter_mut().find(|p| p.id == opponent) {
        // Keep hand size visible but hide card identities
        for &obj_id in &opp.hand {
            if let Some(obj) = filtered.objects.get_mut(&obj_id) {
                obj.face_down = true;
                obj.name = String::from("Hidden Card");
                obj.abilities.clear();
                // Clear all identifying info
            }
        }
    }

    // Shuffle opponent's library order (or just hide contents)
    // Library is already hidden by design -- just don't expose order

    filtered
}
```

### Pattern 4: AI Controller (Alchemy Pattern)
**What:** Client-side controller that schedules AI actions with delays and animation awareness.
**When to use:** VS AI mode in the React client.
**Example (matching Alchemy's aiController.ts):**
```typescript
// client/src/game/controllers/aiController.ts
export function createAIController(
  store: GameStoreAccessor,
  config: AIConfig,
): OpponentController {
  let timeoutId: ReturnType<typeof setTimeout> | null = null;

  const scheduleAIAction = () => {
    if (timeoutId !== null) return;
    const delay = AI_BASE_DELAY + Math.random() * AI_BASE_DELAY;
    timeoutId = setTimeout(() => {
      timeoutId = null;
      // Wait for animations
      if (isAnimating()) { scheduleAIAction(); return; }
      // Check if AI should act
      const { gameState } = store.getState();
      if (!gameState || isGameOver(gameState)) return;
      if (!isAITurn(gameState)) return;
      // Call engine to get AI action (via adapter)
      const action = getAIAction(gameState, config);
      store.dispatch(action);
      // Re-check for multi-step turns
      scheduleAIAction();
    }, delay);
  };

  return {
    onOpponentPhase: () => scheduleAIAction(),
    dispose: () => { if (timeoutId) clearTimeout(timeoutId); },
  };
}
```

### Anti-Patterns to Avoid
- **Exposing full state to clients in multiplayer:** Server must filter before sending -- never trust client to hide info.
- **AI blocking the main thread in WASM:** AI search runs in the engine; for WASM, use tight budget limits (fewer nodes) rather than Web Workers (complexity not needed for turn-based game).
- **Duplicating action validation logic:** `get_legal_actions()` is the single source of truth for both AI and server-side validation.
- **Building separate AI for WASM vs native:** Same Rust code, different config budgets. No conditional compilation needed.

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| WebSocket server | Raw TCP + custom framing | Axum's built-in WebSocket support | Handles upgrade, ping/pong, fragmentation, backpressure |
| Async runtime | Thread pool + channels | Tokio | Battle-tested, required by axum |
| JSON serialization | Manual string building | serde_json | Already used throughout, type-safe |
| Combat evaluation math | New heuristics from scratch | Port Forge's CreatureEvaluator (325 LOC) | 15+ years of tuning, covers edge cases |
| Softmax selection | Manual probability math | Port Alchemy's proven softmaxSelect | Handles numerical stability (max subtraction, etc.) |

**Key insight:** The AI and multiplayer domains have solved-problem components. Forge's combat AI represents 15+ years of MTG-specific tuning. Alchemy's search architecture is battle-tested. Reuse these rather than reinventing.

## Common Pitfalls

### Pitfall 1: State Clone Cost in Game Tree Search
**What goes wrong:** Cloning full GameState (with HashMap<ObjectId, GameObject>) for every search node is expensive.
**Why it happens:** GameState contains HashMap, Vec, String -- all heap-allocated.
**How to avoid:** Start with naive clone (it works), then profile. GameState is already Clone-derived. For MTG's branching factor (~5-20 legal actions per priority), depth 2-3 search with 32-64 nodes is manageable even with full clones. Only optimize if profiling shows it's a bottleneck.
**Warning signs:** AI turn taking >2s on native, >5s on WASM.

### Pitfall 2: Combinatorial Explosion in Attacker/Blocker Enumeration
**What goes wrong:** With 10 creatures, there are 2^10 = 1024 possible attacker subsets. Blocker assignments are even worse.
**Why it happens:** Naive enumeration of all combinations.
**How to avoid:** Use Forge's combat AI approach: score individual attackers/blockers rather than enumerating all subsets. Forge's AiAttackController evaluates each creature independently (should it attack?) then combines. Same for blockers (which blocker assignment minimizes damage?).
**Warning signs:** AI freeze during combat declaration phase.

### Pitfall 3: Hidden Information Leaks in Multiplayer
**What goes wrong:** Serialized GameState accidentally includes opponent's hand card IDs, library order, or pending choices.
**Why it happens:** GameState has all data; easy to miss a field during filtering.
**How to avoid:** Use allowlist approach for filtering (explicitly include safe fields) rather than denylist (strip sensitive fields). Test with a "can opponent see X?" test suite.
**Warning signs:** Players knowing opponent's hand cards.

### Pitfall 4: Reconnection State Divergence
**What goes wrong:** Reconnected player's local state doesn't match server state.
**Why it happens:** Missed events during disconnect, or client state not fully reset.
**How to avoid:** On reconnect, send full filtered GameState snapshot (not delta). Client completely replaces local state. No incremental sync.
**Warning signs:** UI showing stale board state after reconnect.

### Pitfall 5: AI Acting on Opponent's Priority
**What goes wrong:** AI controller fires when it's not the AI's turn to act, or acts when WaitingFor targets a different player.
**Why it happens:** Priority can pass back and forth rapidly; race conditions with setTimeout.
**How to avoid:** Always check `state.waiting_for.player === aiPlayerId` before computing action. The WaitingFor state machine is the single source of truth.
**Warning signs:** Invalid action errors during AI turns.

### Pitfall 6: WASM AI Blocking UI
**What goes wrong:** AI search blocks the WASM thread, freezing the UI.
**Why it happens:** WASM runs on the main thread; long computation = frozen UI.
**How to avoid:** Tight WASM search budgets (max 16-24 nodes vs 64+ native). The simulated thinking delay (0.5-2s) already provides natural pacing -- AI computation should complete well within this window. If needed, break search into chunks with `setTimeout(0)` yielding.
**Warning signs:** UI freezes during AI "thinking."

## Code Examples

### Board Evaluation (Porting Alchemy + Forge patterns)
```rust
// crates/forge-ai/src/eval.rs
// Source: Alchemy's aiEval.ts + Forge's CreatureEvaluator.java

pub struct EvalWeights {
    pub life: f64,
    pub aggression: f64,
    pub board_presence: f64,
    pub board_power: f64,
    pub board_toughness: f64,
    pub hand_size: f64,
}

pub fn evaluate_state(state: &GameState, player: PlayerId, weights: &EvalWeights) -> f64 {
    if let WaitingFor::GameOver { winner } = &state.waiting_for {
        return if *winner == Some(player) { 10000.0 } else { -10000.0 };
    }

    let me = &state.players[player.0 as usize];
    let opp = &state.players[1 - player.0 as usize];

    let life_score = weights.life * me.life as f64 - weights.aggression * opp.life as f64;

    let my_creatures = count_battlefield_creatures(state, player);
    let opp_creatures = count_battlefield_creatures(state, PlayerId(1 - player.0));
    let presence_score = weights.board_presence * (my_creatures - opp_creatures) as f64;

    let (my_power, my_tough) = sum_creature_stats(state, player);
    let (opp_power, opp_tough) = sum_creature_stats(state, PlayerId(1 - player.0));
    let power_score = weights.board_power * (my_power - opp_power) as f64;
    let tough_score = weights.board_toughness * (my_tough - opp_tough) as f64;

    let hand_score = weights.hand_size
        * (me.hand.len() as f64 - opp.hand.len() as f64);

    life_score + presence_score + power_score + tough_score + hand_score
}

/// Creature value incorporating keyword abilities (Forge pattern)
pub fn evaluate_creature(state: &GameState, obj_id: ObjectId) -> f64 {
    let obj = match state.objects.get(&obj_id) { Some(o) => o, None => return 0.0 };
    let power = obj.power.unwrap_or(0) as f64;
    let toughness = obj.toughness.unwrap_or(0) as f64;
    let mut value = power * 1.5 + toughness;

    // Keyword bonuses (Forge's CreatureEvaluator pattern)
    for kw in &obj.keywords {
        match kw.as_str() {
            "Flying" => value += power * 1.0,
            "Trample" => value += power * 0.5,
            "Deathtouch" => value += 3.0,
            "Lifelink" => value += power * 0.5,
            "Hexproof" => value += 2.0,
            "Indestructible" => value += 4.0,
            "First Strike" | "Double Strike" => value += power * 0.8,
            "Vigilance" => value += 1.0,
            "Menace" => value += power * 0.5,
            "Haste" => if obj.entered_battlefield_turn == Some(state.turn_number) { value += 2.0 },
            _ => {}
        }
    }

    if obj.tapped { value -= 1.5; }
    value
}
```

### AI Config (Alchemy pattern, ported to Rust)
```rust
// crates/forge-ai/src/config.rs
// Source: Alchemy's aiConfig.ts

#[derive(Clone)]
pub struct AiConfig {
    pub difficulty: AiDifficulty,
    pub temperature: f64,
    pub play_lookahead: bool,
    pub combat_lookahead: bool,
    pub search: SearchConfig,
    pub weights: EvalWeights,
}

#[derive(Clone)]
pub struct SearchConfig {
    pub enabled: bool,
    pub max_depth: u32,
    pub max_nodes: u32,
    pub max_branching: u32,
}

pub fn create_config(difficulty: AiDifficulty, platform: Platform) -> AiConfig {
    let base = match difficulty {
        AiDifficulty::VeryEasy => AiConfig {
            temperature: 4.0,
            play_lookahead: false,
            combat_lookahead: false,
            search: SearchConfig { enabled: false, max_depth: 0, max_nodes: 0, max_branching: 0 },
            ..Default::default()
        },
        AiDifficulty::Easy => AiConfig {
            temperature: 2.0,
            play_lookahead: true,
            combat_lookahead: false,
            search: SearchConfig { enabled: false, ..Default::default() },
            ..Default::default()
        },
        AiDifficulty::Medium => AiConfig {
            temperature: 1.0,
            search: SearchConfig { enabled: true, max_depth: 2, max_nodes: 24, max_branching: 5 },
            ..Default::default()
        },
        AiDifficulty::Hard => AiConfig {
            temperature: 0.5,
            search: SearchConfig { enabled: true, max_depth: 3, max_nodes: 48, max_branching: 5 },
            ..Default::default()
        },
        AiDifficulty::VeryHard => AiConfig {
            temperature: 0.01,
            search: SearchConfig { enabled: true, max_depth: 3, max_nodes: 64, max_branching: 6 },
            ..Default::default()
        },
    };

    // WASM gets tighter budgets
    match platform {
        Platform::Wasm => AiConfig {
            search: SearchConfig {
                max_depth: base.search.max_depth.min(2),
                max_nodes: (base.search.max_nodes * 2 / 3).max(1),
                ..base.search
            },
            ..base
        },
        Platform::Native => base,
    }
}
```

### WebSocket Protocol Messages
```rust
// crates/server-core/src/protocol.rs

#[derive(Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClientMessage {
    /// Create a new game session (host)
    CreateGame { deck: DeckData },
    /// Join an existing game session
    JoinGame { game_code: String, deck: DeckData },
    /// Submit a game action
    Action { action: GameAction },
    /// Request reconnection
    Reconnect { game_code: String, player_token: String },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServerMessage {
    /// Game created, waiting for opponent
    GameCreated { game_code: String, player_token: String },
    /// Game started, initial state
    GameStarted { state: GameState, your_player: PlayerId },
    /// State update after an action
    StateUpdate { state: GameState, events: Vec<GameEvent> },
    /// Action rejected
    ActionRejected { reason: String },
    /// Opponent disconnected
    OpponentDisconnected { grace_seconds: u32 },
    /// Opponent reconnected
    OpponentReconnected,
    /// Game over (timeout forfeit or normal end)
    GameOver { winner: Option<PlayerId>, reason: String },
    /// Error
    Error { message: String },
}
```

### Axum WebSocket Server Setup
```rust
// crates/forge-server/src/main.rs

use axum::{
    extract::ws::{WebSocket, WebSocketUpgrade},
    routing::get,
    Router,
};

async fn ws_handler(ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(handle_socket)
}

async fn handle_socket(mut socket: WebSocket) {
    while let Some(Ok(msg)) = socket.recv().await {
        if let Message::Text(text) = msg {
            let client_msg: ClientMessage = serde_json::from_str(&text)?;
            // Route to session manager
        }
    }
}

#[tokio::main]
async fn main() {
    let app = Router::new()
        .route("/ws", get(ws_handler))
        .route("/health", get(|| async { "ok" }));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
```

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| tokio-tungstenite + manual routing | Axum native WebSocket | axum 0.7+ (2024) | Simpler server code, built-in upgrade handling |
| Separate WASM/native AI code | Same Rust source, cfg-based budgets | Rust WASM maturity (2023+) | Single codebase, no divergence |
| Forge's OOP ComputerPlayer hierarchy | Functional approach with pure evaluation | This project's architecture | Aligns with engine's pure apply() pattern |

## Open Questions

1. **Attacker subset enumeration strategy**
   - What we know: Forge evaluates each creature independently (should attack?). Naive subset enumeration explodes at ~10 creatures.
   - What's unclear: Exact threshold where Forge's individual-creature approach is sufficient vs. needing subset optimization
   - Recommendation: Start with Forge's individual evaluation approach. Only consider subset search if testing shows poor attack decisions.

2. **WASM AI performance**
   - What we know: WASM is ~1.5-2x slower than native for compute. GameState clone involves heap allocations.
   - What's unclear: Exact node budget that stays under 500ms on typical tablet hardware.
   - Recommendation: Start with 16 nodes for WASM medium, 24 for hard. Profile on actual tablet and adjust.

3. **Standard card list source**
   - What we know: Need current Standard-legal set list to determine which .txt files to analyze.
   - What's unclear: Best source for programmatic Standard legality list.
   - Recommendation: Hardcode current Standard set codes (e.g., MKM, OTJ, BLB, DSK, FDN, etc.) -- Standard rotations are infrequent and well-known.

## Validation Architecture

### Test Framework
| Property | Value |
|----------|-------|
| Framework | Rust unit tests (cargo test) + Vitest (client) |
| Config file | Cargo.toml (Rust), vitest.config.ts (client) |
| Quick run command | `cargo test -p forge-ai --lib` |
| Full suite command | `cargo test --workspace && cd client && pnpm test` |

### Phase Requirements -> Test Map
| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| AI-01 | Legal action enumeration produces valid actions for each WaitingFor variant | unit | `cargo test -p forge-ai legal_actions -x` | Wave 0 |
| AI-02 | Board evaluation returns higher scores for advantageous positions | unit | `cargo test -p forge-ai eval -x` | Wave 0 |
| AI-03 | Per-card ability hints affect play decisions (e.g., removal held for threats) | unit | `cargo test -p forge-ai card_hints -x` | Wave 0 |
| AI-04 | Tree search finds better moves than random at depth 2+ | unit | `cargo test -p forge-ai search -x` | Wave 0 |
| AI-05 | Difficulty levels produce different move quality distributions | unit | `cargo test -p forge-ai config -x` | Wave 0 |
| MP-01 | Server accepts WebSocket connections and routes messages | integration | `cargo test -p forge-server -- --ignored` | Wave 0 |
| MP-02 | Filtered state hides opponent hand and library order | unit | `cargo test -p server-core filter -x` | Wave 0 |
| MP-03 | Server validates actions and rejects illegal moves | unit | `cargo test -p server-core session -x` | Wave 0 |
| MP-04 | Reconnection restores game state within grace period | integration | `cargo test -p server-core reconnect -x` | Wave 0 |
| PLAT-05 | Coverage analysis correctly identifies supported/unsupported cards | unit | `cargo test -p engine coverage -x` | Wave 0 |

### Sampling Rate
- **Per task commit:** `cargo test -p forge-ai --lib` or `cargo test -p server-core --lib`
- **Per wave merge:** `cargo test --workspace`
- **Phase gate:** Full suite green before `/gsd:verify-work`

### Wave 0 Gaps
- [ ] `crates/forge-ai/` -- entire new crate (lib.rs, legal_actions.rs, eval.rs, search.rs, config.rs, combat_ai.rs)
- [ ] `crates/server-core/` -- entire new crate (lib.rs, session.rs, protocol.rs, filter.rs, reconnect.rs)
- [ ] `crates/forge-server/` -- entire new crate (main.rs)
- [ ] Cargo workspace members updated to include new crates
- [ ] `client/src/game/controllers/` -- aiController.ts
- [ ] `client/src/adapter/ws-adapter.ts` -- WebSocketAdapter

## Sources

### Primary (HIGH confidence)
- Alchemy project (`../alchemy/src/engine/ai*.ts`, `../alchemy/src/game/controllers/aiController.ts`) -- proven AI architecture with 5 difficulty levels, alpha-beta search, softmax selection, AI controller pattern
- Forge project (`../forge/forge-ai/src/main/java/forge/ai/`) -- CreatureEvaluator (325 LOC), AiAttackController (1770 LOC), AiBlockController (1378 LOC), ComputerUtilCombat (2609 LOC) reference implementations
- Engine crate source code -- apply() function, GameState/WaitingFor/GameAction types, existing architecture patterns
- Client source code -- EngineAdapter interface, WasmAdapter/TauriAdapter implementations, gameStore, MenuPage

### Secondary (MEDIUM confidence)
- [Axum WebSocket documentation](https://docs.rs/axum/latest/axum/extract/ws/index.html) -- native WebSocket support in axum 0.7+
- [tokio-tungstenite GitHub](https://github.com/snapview/tokio-tungstenite) -- underlying WebSocket implementation

### Tertiary (LOW confidence)
- WASM performance budgets -- estimated from general WASM overhead (~1.5-2x native); needs profiling on actual hardware

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH -- Axum/tokio is the dominant Rust web stack; AI architecture proven in Alchemy
- Architecture: HIGH -- Crate structure follows established patterns; EngineAdapter interface already supports new adapters
- Pitfalls: HIGH -- Combat explosion and hidden info leaks are well-documented game AI/multiplayer issues
- Card coverage: MEDIUM -- Gap analysis approach is sound but exact Standard set list needs verification

**Research date:** 2026-03-08
**Valid until:** 2026-04-08 (stable domain -- AI algorithms and WebSocket protocols don't change)
