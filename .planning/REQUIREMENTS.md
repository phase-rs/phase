# Requirements: Forge.rs

**Defined:** 2026-03-10
**Core Value:** A player can sit down, pick a Standard-legal deck, and play a full game of Magic against a competent AI opponent — with all cards behaving correctly according to MTG comprehensive rules.

## v1.2 Requirements

Requirements for v1.2 milestone. Each maps to roadmap phases.

### Data Pipeline

- [x] **DATA-01**: Engine loads card metadata (name, mana cost, types, P/T, colors, keywords, oracle text, layout) from MTGJSON AllCards AtomicCards.json using custom Rust types
- [x] **DATA-02**: Engine defines a typed JSON ability schema mapping to AbilityDefinition, TriggerDefinition, StaticDefinition, and ReplacementDefinition types
- [x] **DATA-03**: CardDatabase::load_json() merges MTGJSON metadata + ability JSON into CardFace, becoming the primary card loading path
- [x] **DATA-04**: Ability JSON schema exports a JSON Schema definition via schemars for editor autocompletion and build-time validation

### Card Migration

- [x] **MIGR-01**: All engine-supported cards (thousands — every card whose mechanics have registered handlers) are converted from Forge .txt to MTGJSON metadata + ability JSON via automated migration
- [x] **MIGR-02**: data/cardsfolder/ and data/standard-cards/ are removed from the repository; Forge parser is feature-gated behind forge-compat
- [x] **MIGR-03**: Automated Forge-to-JSON migration tool converts all 32,300+ Forge .txt card definitions to the new ability JSON format, producing ability files for every engine-supported card
- [x] **MIGR-04**: Card data includes MTGJSON scryfallOracleId for reliable frontend image lookups via Scryfall API
- [x] **MIGR-05**: CI coverage gate updated to validate against JSON card data; all previously supported cards remain supported after migration

### Testing

- [x] **TEST-01**: A self-contained GameScenario test harness provides add_card(), set_phase(), act(), and assertion helpers with no external filesystem dependencies
- [x] **TEST-02**: Scenario-based rules correctness tests cover core mechanics: ETB triggers, combat, stack resolution, state-based actions, layer system, keyword interactions
- [x] **TEST-03**: insta snapshot tests capture GameState after action sequences to detect unintended engine changes across commits
- [x] **TEST-04**: Per-card behavioral parity tests confirm migrated cards produce identical game outcomes as the original Forge format (sampled across mechanics, not exhaustive per-card)

### Licensing & Cleanup

- [x] **LICN-01**: Project relicensed as MIT/Apache-2.0 dual license after all GPL-coupled data is removed
- [x] **LICN-02**: PROJECT.md constraints and key decisions updated to reflect MTGJSON + own ability format (removing Forge format dependency)
- [x] **LICN-03**: Coverage report (coverage.rs) reads JSON format and CI gate (100% Standard coverage) is preserved

## Phase 26 Requirements

Requirements for multiplayer polish phase. Maps to Phase 26.

### Bug Fixes

- [x] **MP-BUG-A**: Starting a new online game clears stale session data, preventing reconnect to dead sessions
- [x] **MP-BUG-B**: Deck selection is validated before entering an online game (same guard as AI path)
- [x] **MP-BUG-C**: Opponent actions are visible in real-time — StateUpdate without pendingResolve updates game store directly
- [x] **MP-BUG-D**: WebSocketAdapter.getAiAction() returns null instead of throwing in multiplayer
- [x] **MP-BUG-E**: Player ID is dynamic from server's GameStarted message, not hardcoded to 0

### Player Identity

- [x] **MP-IDENT**: Player has a persistent UUID identity and display name stored in localStorage, with a Zustand multiplayerStore

### Server Lobby

- [x] **MP-LOBBY-SRV**: Server manages a browseable game lobby with create/list/join/subscribe/unsubscribe protocol, public/private toggle, password protection, and auto-expiry of stale games
- [x] **MP-CONCEDE-SRV**: Server handles Concede message and broadcasts game over to both players
- [x] **MP-EMOTE-SRV**: Server forwards Emote messages between players
- [x] **MP-TIMER-SRV**: Server supports per-turn timer configuration and enforcement with TimerUpdate messages

### Frontend Lobby

- [x] **MP-LOBBY-UI**: Frontend displays a browseable game list with real-time updates, manual code entry, and player count
- [x] **MP-MENU-FLOW**: Menu state machine follows mode-select -> deck-gallery-online -> lobby -> host-setup -> waiting
- [x] **MP-HOST-SETUP**: Host setup screen captures display name, public/private, password, format, and timer settings
- [x] **MP-WAITING**: Waiting screen shows game code, LAN IP, listed-in-lobby status, and cancel button
- [x] **MP-SETTINGS**: Dedicated Multiplayer section in settings for server address and display name

### P2P WebRTC

- [x] **MP-P2P**: PeerJS-based WebRTC networking layer ported from Alchemy project with TURN/signaling support
- [x] **MP-P2P-HOST**: P2PHostAdapter runs WASM engine locally and sends filtered state to guest via DataChannel
- [x] **MP-P2P-GUEST**: P2PGuestAdapter receives state from host and sends actions, code-only (no lobby listing)

### Desktop Hosting

- [x] **MP-SIDECAR**: phase-server embedded as Tauri sidecar binary, spawned on Host Game, auto-stops when game ends
- [x] **MP-CONNECT-UX**: Connection status dot (green/yellow/red) visible throughout multiplayer, toast on failure with Retry/Settings
- [x] **MP-SERVER-DETECT**: Smart server detection: (1) Tauri sidecar localhost, (2) last-used server, (3) manual entry; CODE@IP:PORT join syntax

### In-Game UX

- [x] **MP-CONCEDE**: Player can concede via game menu with confirmation dialog
- [x] **MP-EMOTE**: Quick emotes (MTGA-style pre-set messages) can be sent and received with temporary overlay display
- [x] **MP-TIMER-UI**: Per-turn timer countdown displayed near active player HUD when timer is configured
- [x] **MP-GAMEOVER**: Game over screen shows correct winner for both players, total turns, duration, and Back to Lobby button
- [x] **MP-OPPONENT-NAME**: Opponent display name shown near their life total during gameplay

## Future Requirements

Deferred to v2+. Tracked but not in current roadmap.

### Testing (Advanced)

- **TEST-05**: Property-based testing with proptest for randomized game state exploration
- **TEST-06**: Comprehensive rules reference tests indexed by MTG Comprehensive Rule number

### Card Expansion

- **EXPN-01**: MTGJSON auto-update script for new set releases

## Out of Scope

| Feature | Reason |
|---------|--------|
| Natural language ability parsing from oracle text | NLP problem far beyond this milestone's scope |
| Runtime migration to Vec\<AbilityDefinition\> on GameObject | Research identified as cleaner long-term, but v1.2 can emit Forge-compatible strings from JSON loader — defer refactor to avoid touching ~13 source files |
| Full git history rewrite to remove GPL files | git filter-branch is destructive; .gitignore + deletion sufficient for licensing purposes |
| Manual ability authoring for unsupported cards | Migration tool handles supported cards; adding new handler coverage is separate work |
| Cross-device player identity / accounts | Requires auth infrastructure, separate phase |
| Friends list / friend invites | Requires persistent accounts, separate phase |
| Spectator mode | Requires spectator state filter, separate phase |
| Rematch button | Requires protocol extension, future enhancement |
| Detailed game statistics | Future enhancement |
| Turn timer with time bank | Alternative timer model, future enhancement |

## Traceability

Which phases cover which requirements. Updated during roadmap creation.

| Requirement | Phase | Status |
|-------------|-------|--------|
| DATA-01 | Phase 21 | Complete |
| DATA-02 | Phase 21 | **Complete** (21-01) |
| DATA-03 | Phase 23 | Complete |
| DATA-04 | Phase 21 | Complete |
| MIGR-01 | Phase 24 | Complete |
| MIGR-02 | Phase 25 | Complete |
| MIGR-03 | Phase 24 | Complete |
| MIGR-04 | Phase 23 | Complete |
| MIGR-05 | Phase 24 | Complete |
| TEST-01 | Phase 22 | Complete |
| TEST-02 | Phase 22 | Complete |
| TEST-03 | Phase 22 | Complete |
| TEST-04 | Phase 24 | Complete |
| LICN-01 | Phase 25 | Complete |
| LICN-02 | Phase 25 | Complete |
| LICN-03 | Phase 25 | Complete |
| MP-BUG-A | Phase 26 | Planned |
| MP-BUG-B | Phase 26 | Planned |
| MP-BUG-C | Phase 26 | Planned |
| MP-BUG-D | Phase 26 | Planned |
| MP-BUG-E | Phase 26 | Planned |
| MP-IDENT | Phase 26 | Planned |
| MP-LOBBY-SRV | Phase 26 | Planned |
| MP-CONCEDE-SRV | Phase 26 | Planned |
| MP-EMOTE-SRV | Phase 26 | Planned |
| MP-TIMER-SRV | Phase 26 | Planned |
| MP-LOBBY-UI | Phase 26 | Planned |
| MP-MENU-FLOW | Phase 26 | Planned |
| MP-HOST-SETUP | Phase 26 | Planned |
| MP-WAITING | Phase 26 | Planned |
| MP-SETTINGS | Phase 26 | Planned |
| MP-P2P | Phase 26 | Planned |
| MP-P2P-HOST | Phase 26 | Planned |
| MP-P2P-GUEST | Phase 26 | Planned |
| MP-SIDECAR | Phase 26 | Planned |
| MP-CONNECT-UX | Phase 26 | Planned |
| MP-SERVER-DETECT | Phase 26 | Planned |
| MP-CONCEDE | Phase 26 | Planned |
| MP-EMOTE | Phase 26 | Planned |
| MP-TIMER-UI | Phase 26 | Planned |
| MP-GAMEOVER | Phase 26 | Planned |
| MP-OPPONENT-NAME | Phase 26 | Planned |

**Coverage:**
- v1.2 requirements: 16 total (all complete)
- Phase 26 requirements: 25 total
- Unmapped: 0

---
*Requirements defined: 2026-03-10*
*Last updated: 2026-03-10 after Phase 26 planning*
