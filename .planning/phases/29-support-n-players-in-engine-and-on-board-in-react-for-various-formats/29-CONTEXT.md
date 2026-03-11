# Phase 29: Support N Players in Engine and on Board in React for Various Formats - Context

**Gathered:** 2026-03-11
**Status:** Ready for planning

<domain>
## Phase Boundary

Extend the engine's hardcoded 2-player model to support N players, update the board UI to render multiple player areas, introduce format-awareness (Standard, Commander, Free-for-All, Two-Headed Giant), and update networking/lobby/deck builder/AI for multiplayer formats. XMage (../mage, MIT) serves as architecture reference and test case source — but idiomatic Rust and clean architecture are the primary design principles.

</domain>

<decisions>
## Implementation Decisions

### Format Scope
- Support four formats: Standard (1v1, 20 life), Commander/EDH (2-6 players, 40 life), Free-for-All (2-6 players, 20 life), Two-Headed Giant (4 players, 2v2, shared 30 life per team)
- Full Commander rules: command zone, commander tax (+2 per cast), commander damage tracking (21 from one commander = loss), color identity enforcement, 100-card singleton, partner commander support (two commanders, combined color identity, separate damage tracking)
- Proper Two-Headed Giant simultaneous turns (team members share a turn and combat phase, team-based priority passing)
- Full MTG elimination rules (CR 800.4): eliminated player's permanents exiled, spells removed from stack, effects end. Last player/team standing wins
- Full deck validation per format at game start (reject invalid decks with clear error messages)
- `FormatConfig` enum + struct: `GameFormat` enum (Standard, Commander, FFA, TwoHeadedGiant) maps to `FormatConfig` struct (starting_life, min/max_players, deck_size, singleton, command_zone, commander_damage_threshold, range_of_influence, team_based)
- Optional range of influence field in game config (MTG CR 801) — rarely used but part of the rules
- Format rule overrides: host can customize starting life, commander damage threshold, etc. ("house rules")

### Board Layout
- Unified `PlayerArea` component with `mode` prop: `full` (your area), `focused` (expanded opponent), `compact` (condensed opponent)
- **1v1 visual parity is a hard requirement**: in 2-player games, the single opponent defaults to `focused` mode, reproducing the exact current top/bottom layout
- 3+ players: all opponents in compact mode by default. Compact strips show small art-crop permanents (grouped: creatures, lands, other) + life total + hand size + commander damage
- Optional focus: click an opponent to expand to focused mode (showing full board). Auto-focus during interaction (combat targeting, spell targeting). Focus is never required — all opponents can stay compact
- Commander card displayed in each player's strip with a special visual highlight (border glow, badge, or similar) to make commanders instantly identifiable
- Commander damage from each source tracked and displayed in player info area

### Turn & Priority
- Strict MTG multiplayer rules: clockwise turn rotation, APNAP priority order
- Explicit `seat_order: Vec<PlayerId>` on GameState for randomized seating independent of player IDs
- Priority passes clockwise from active player; all living players must pass consecutively for stack resolution (replacing hardcoded `priority_pass_count >= 2`)
- `opponent()` function redesign: Claude's discretion on API shape based on analysis of 57 files / 764+ usages — some callers need "all others", some need "next in turn order", some need "specific target"
- Simultaneous trigger resolution in APNAP order

### Combat Targeting
- Per-creature attack target selection: each attacking creature independently chooses its target (player or planeswalker). Enables splitting attacks across multiple opponents
- "Attack all" convenience button that sends all eligible creatures at a selected target, plus per-creature override for splitting
- Full board visible during blocking: all combats visible, defenders can only assign blockers to creatures attacking them, UI highlights legal block targets

### Multiplayer Networking
- WebSocket server only for 3+ player games (P2P stays 2-player only)
- Lobby with ready-up system: format-aware lobby shows format, current/max players, join fills a seat. Players join and mark ready. Host starts when enough players are ready
- Start rules: Commander/FFA can start with 2+ players (no forced upper limit). 2HG requires exactly 4 (2 per team). Standard requires exactly 2
- Pause game on disconnect: when any human disconnects, pause game for all players. Resume on reconnect or AI takeover after 2-minute timeout
- State filtering: each player sees own hand cards + all public zones. Opponent hand sizes (card count) visible. Library contents hidden
- Spectator support: spectators join and watch with hidden-info view (public zones only, no hands). Eliminated players auto-transition to spectator

### AI in Multiplayer
- AI fills any empty seat: enables solo Commander pods (1 human + 3 AI), mixed pods, etc.
- Per-seat difficulty configuration: host picks difficulty per AI seat independently
- Threat-aware AI: tracks threat levels per opponent (board state, life, commander damage), targets biggest threat, avoids piling on weakest player
- AI deck assignment: host can pick specific decks OR choose "random" for AI seats (both options)
- AI search strategy: researcher investigates best approach (adapt alpha-beta for N-player, MCTS, hybrid). Priority is capable AI — not just functional but smart. Current engine uses alpha-beta with iterative deepening
- AI search budget scaling for multiplayer: adjust depth/nodes based on player count and platform (WASM constraints)

### Deck Builder Updates
- Full Commander support: commander designation in deck builder, 100-card singleton enforcement, color identity validation (block off-color cards with explanation), partner commander support
- Format legality badges per card (from MTGJSON data): show "Legal in Standard", "Legal in Commander" etc.
- Format filter in card search
- Import with auto-detect: import .dck/.dec files, auto-detect commander from sideboard slot
- Pre-built Commander decks (4-5 precons covering different color combinations) for immediate play
- Deck builder format access: Claude's discretion on which formats appear as build targets

### Game Setup Flow
- Format-first flow: pick format via big buttons (Standard, Commander, FFA, 2HG), then format-specific config screen
- Commander deck selection: pick from existing Commander decks (commander shown on deck tile art). No separate commander picker step
- Pre-game lobby room: player list (name, deck name, ready status), chat, ready toggle. Host sees "Start Game" button when enough players ready
- Game preset saving: save configured game setup as preset for quick-start next time
- Format rule overrides available in setup (starting life, commander damage threshold, etc.)

### Claude's Discretion
- Exact `opponent()` / `opponents()` / `next_player()` API design based on usage analysis
- AI search algorithm choice (alpha-beta adaptation vs MCTS vs hybrid) — researcher investigates
- Deck builder format access UX (which formats show as build targets)
- Compact strip exact layout and card sizing within existing CSS custom property system
- Commander visual highlight style (glow, badge, icon)
- Pre-game lobby room chat implementation details

</decisions>

<specifics>
## Specific Ideas

- "Idiomatic Rust and clean architecture are the PRIMARY rule — XMage is reference only, never direct port"
- Unified PlayerArea component must look identical to current 2-player layout when opponent is in focused mode (1v1 visual parity)
- User wants "capable AI" in multiplayer — not just functional but threat-aware and strategically sound
- Commander games should be playable with as few as 2 players (1v1 Commander)
- "Attack all" button for convenience, with per-creature override for splitting attacks
- XMage (../mage) checked out locally at ../mage for architecture reference and test case source
- Format-first game setup feels like best UX (pick format → see format-specific config)

</specifics>

<code_context>
## Existing Code Insights

### Reusable Assets
- `PlayerId(pub u8)` and `players: Vec<Player>` already support N players at the type level
- `GameBoard.tsx` with top/bottom split — needs refactoring into unified PlayerArea
- `phase-ai` with alpha-beta search + eval + card_hints — needs N-player adaptation
- `filter_state_for_player` in server — needs extension for N players
- `multiplayerStore` with lobby, game code, timer — needs format/player count fields
- `LobbyView`, `HostSetup`, `WaitingScreen` components — need format-aware upgrades
- `deckParser.ts` — needs commander auto-detection from sideboard

### Established Patterns
- Pure `apply(state, action) -> ActionResult` reducer — N-player is a state shape change, not architecture change
- Discriminated unions with `#[serde(tag = "type")]` for all engine types — new format/combat types follow same pattern
- CSS custom property card sizing system — PlayerArea density modes can leverage this
- `EngineAdapter` interface (WASM, Tauri, WebSocket) — format config flows through existing adapter interface
- `FormatConfig` struct follows existing typed enum patterns (like `Effect`, `TriggerMode`)

### Integration Points
- `GameState::new_two_player()` → needs `GameState::new(format_config, player_count, seed)` factory
- `opponent()` in `priority.rs` → used in 57 files, 764+ occurrences — systematic replacement needed
- `priority_pass_count >= 2` → needs `priority_pass_count >= living_players.len()`
- `defending_player = PlayerId(1 - active_player.0)` in combat → per-creature attack target
- `get_ai_action()` in WASM bridge → needs player_id parameter instead of hardcoded PlayerId(1)
- `DeclareAttackers` action → needs per-creature target assignment field
- `CombatState.defending_player: PlayerId` → needs `per_creature_targets: HashMap<ObjectId, AttackTarget>`

</code_context>

<deferred>
## Deferred Ideas

- Draft/Sealed game modes — different kind of feature (deck construction before gameplay), belongs in its own phase
- Brawl format (60-card Standard-legal Commander variant) — could be added as another GameFormat variant later
- Archenemy format — specialized multiplayer variant, future phase
- Planechase — specialized multiplayer variant, future phase
- P2P multiplayer for 3+ players (WebRTC mesh/star) — complex, server-only for now
- Mobile-optimized multiplayer layout — responsive design for smaller screens
- Replay/spectator replay system — watch completed games

</deferred>

---

*Phase: 29-support-n-players-in-engine-and-on-board-in-react-for-various-formats*
*Context gathered: 2026-03-11*
