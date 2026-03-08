# Requirements: Forge.rs v1.1 Arena UI

**Defined:** 2026-03-08
**Core Value:** A player can sit down, pick a Standard-legal deck, and play a full game of Magic against a competent AI opponent -- with all cards behaving correctly according to MTG comprehensive rules.

## v1.1 Requirements

Requirements for Arena UI port. Each maps to roadmap phases.

### Board Layout

- [ ] **BOARD-01**: Game board uses CSS custom properties for responsive card sizing across mobile, tablet, and desktop breakpoints
- [ ] **BOARD-02**: Battlefield renders permanents in dynamic multi-row layout grouped by type (creatures, non-creatures, lands)
- [ ] **BOARD-03**: Same-name tokens stack with count badge, expandable on click
- [ ] **BOARD-04**: Same-name lands group together with count badge
- [ ] **BOARD-05**: Tapped permanents render at 90 degree rotation
- [ ] **BOARD-06**: Auras and equipment render visually attached to their host permanent
- [ ] **BOARD-07**: Counter overlays display on permanents (e.g. +1/+1, loyalty) with count
- [ ] **BOARD-08**: Persistent damage number displays on damaged creatures
- [ ] **BOARD-09**: Battlefield backgrounds auto-select based on player's dominant mana color (WUBRG) using Forge assets

### Hand

- [ ] **HAND-01**: Player hand uses MTGA-style fan layout with peek/expand from bottom edge
- [ ] **HAND-02**: Cards in hand support drag-to-play interaction with threshold to prevent accidental drags
- [ ] **HAND-03**: Playable cards highlight based on legal actions from engine
- [ ] **HAND-04**: Opponent hand displays card backs in compact fan

### Player HUD

- [ ] **HUD-01**: Player HUD displays life total, mana pool summary, and active phase indicator
- [ ] **HUD-02**: Opponent HUD displays life total and mana pool summary
- [ ] **HUD-03**: Life total animates on change (damage flash red, gain flash green)

### Animation Pipeline

- [ ] **ANIM-01**: Step-based animation queue processes engine events sequentially with configurable timing
- [ ] **ANIM-02**: Event normalizer translates Forge.rs GameEvent types to animation-compatible format
- [ ] **ANIM-03**: Board snapshot system preserves dying creatures visually during death animation sequence
- [ ] **ANIM-04**: Async dispatch wrapper captures positions before WASM call and serializes dispatch-animate flow
- [ ] **ANIM-05**: VFX quality levels (full/reduced/minimal) configurable in preferences
- [ ] **ANIM-06**: Animation speed slider configurable in preferences

### Visual Effects

- [ ] **VFX-01**: Canvas particle system renders 9+ effect types (explosion, projectile, spellImpact, etc.) with WUBRG color mapping
- [ ] **VFX-02**: Floating damage/heal numbers animate with scale-in, float-up, fade-out per step
- [ ] **VFX-03**: Screen shake triggers on combat damage at 3 intensity levels
- [ ] **VFX-04**: Card reveal animation plays on creature/spell entry with burst effect
- [ ] **VFX-05**: Damage vignette (red screen flash) on player damage
- [ ] **VFX-06**: Block assignment lines (SVG) connect attacker/blocker pairs during combat
- [ ] **VFX-07**: Targeting arcs connect spells/abilities to their targets during resolution
- [ ] **VFX-08**: Turn/phase banner overlay animates on phase transitions

### Audio

- [ ] **AUDIO-01**: SFX play on game events using Forge's 39 sound effect assets via Web Audio API
- [ ] **AUDIO-02**: Background music plays during matches using Forge's battle music tracks (CC-BY 3.0)
- [ ] **AUDIO-03**: Music auto-selects WUBRG-themed tracks based on player's deck colors when available
- [ ] **AUDIO-04**: Independent volume controls for SFX and music with mute toggles
- [ ] **AUDIO-05**: iOS/iPadOS AudioContext warm-up on first user interaction

### Game Loop

- [ ] **LOOP-01**: OpponentController abstraction supports AI (via WASM) and network (via WebSocket) opponents
- [ ] **LOOP-02**: useGameLoop hook auto-advances game phases, waits for animations, and delegates to controller
- [ ] **LOOP-03**: GameDispatchProvider context provides dispatch + controller to all components (no prop drilling)
- [ ] **LOOP-04**: Auto-priority-pass skips trivial priority windows (e.g. upkeep with no triggers, empty stack)

### Stack & Priority

- [ ] **STACK-01**: Arena-style stack visualization displays spells/abilities waiting to resolve with card art and description
- [ ] **STACK-02**: Priority pass/respond buttons appear when player has priority
- [ ] **STACK-03**: Auto-pass toggle allows skipping priority when no legal instant-speed actions available
- [ ] **STACK-04**: Full-control mode disables all auto-passing for manual control of every priority window

### Mana Payment

- [ ] **MANA-01**: Mana payment UI displays required cost with WUBRG symbols and allows manual color selection
- [ ] **MANA-02**: Mana payment handles hybrid, phyrexian, and X costs with appropriate UI affordances
- [ ] **MANA-03**: Mana pool display updates in real-time as mana is produced and spent

### Combat

- [ ] **COMBAT-01**: Attacker declaration UI highlights legal attackers and supports click-to-toggle
- [ ] **COMBAT-02**: Blocker declaration UI highlights legal blockers and supports click-to-assign
- [ ] **COMBAT-03**: Combat math bubbles preview P/T trade outcomes before damage resolution
- [ ] **COMBAT-04**: Combat damage assignment modal distributes damage across multiple blockers (trample, multi-block)

### Zones

- [ ] **ZONE-01**: Graveyard viewer modal displays all cards in graveyard with scrolling
- [ ] **ZONE-02**: Exile zone viewer displays exiled cards
- [ ] **ZONE-03**: Zone card counts display on zone indicators (e.g. "Graveyard (7)")

### Game Log

- [ ] **LOG-01**: Scrollable game log panel displays game events in chronological order
- [ ] **LOG-02**: Log entries are color-coded by event type (combat, spells, life changes, etc.)
- [ ] **LOG-03**: Log verbosity is filterable (full/compact/minimal)

### Integration

- [ ] **INTEG-01**: All UI components wire through EngineAdapter interface preserving WASM, Tauri, and WebSocket support
- [ ] **INTEG-02**: GameObject view model maps deep Forge.rs engine objects to flat props for UI components
- [ ] **INTEG-03**: Preferences store persists display, audio, and gameplay settings to localStorage

## v2 Requirements

Deferred to future release. Tracked but not in current roadmap.

### Polish

- **POLISH-01**: Foil effect overlay on foil-edition cards
- **POLISH-02**: Context-specific music (title screen, deck select, different for each game mode)
- **POLISH-03**: Custom UI layout configuration
- **POLISH-04**: Element card effects on permanents (subtle VFX, full quality only)
- **POLISH-05**: Library viewer (top N cards when allowed by game rules)

### Educational

- **EDU-01**: Tutorial system for new players
- **EDU-02**: Coach overlay with contextual gameplay tips

## Out of Scope

| Feature | Reason |
|---------|--------|
| Alchemy game engine | Using Rust/WASM engine |
| Alchemy card/effect registries | Using Forge card database |
| Alchemy AI system | Using Rust forge-ai crate |
| Alchemy network layer (PeerJS) | Using Forge.rs WebSocket server |
| Learning challenges | Educational feature, not relevant to MTG |
| Campaign/adventure mode | Narrative framework not core to gameplay |
| Tutorial system | Deferred to v2 |
| Easy-read mode / TTS | Not relevant to MTG gameplay |
| Alchemy synthesis audio | Using Forge Java's proven SFX/music assets |
| Deck builder redesign | Keeping existing Forge.rs deck builder for now |
| MultiplayerLobby from Alchemy | Using Forge.rs WebSocket approach |

## Traceability

Which phases cover which requirements. Updated during roadmap creation.

| Requirement | Phase | Status |
|-------------|-------|--------|
| BOARD-01 | Phase 13 | Pending |
| BOARD-02 | Phase 13 | Pending |
| BOARD-03 | Phase 13 | Pending |
| BOARD-04 | Phase 13 | Pending |
| BOARD-05 | Phase 13 | Pending |
| BOARD-06 | Phase 13 | Pending |
| BOARD-07 | Phase 13 | Pending |
| BOARD-08 | Phase 13 | Pending |
| BOARD-09 | Phase 13 | Pending |
| HAND-01 | Phase 13 | Pending |
| HAND-02 | Phase 13 | Pending |
| HAND-03 | Phase 13 | Pending |
| HAND-04 | Phase 13 | Pending |
| HUD-01 | Phase 13 | Pending |
| HUD-02 | Phase 13 | Pending |
| HUD-03 | Phase 13 | Pending |
| ZONE-01 | Phase 13 | Pending |
| ZONE-02 | Phase 13 | Pending |
| ZONE-03 | Phase 13 | Pending |
| LOG-01 | Phase 13 | Pending |
| LOG-02 | Phase 13 | Pending |
| LOG-03 | Phase 13 | Pending |
| INTEG-01 | Phase 13 | Pending |
| INTEG-02 | Phase 13 | Pending |
| INTEG-03 | Phase 13 | Pending |
| ANIM-01 | Phase 14 | Pending |
| ANIM-02 | Phase 14 | Pending |
| ANIM-03 | Phase 14 | Pending |
| ANIM-04 | Phase 14 | Pending |
| ANIM-05 | Phase 14 | Pending |
| ANIM-06 | Phase 14 | Pending |
| VFX-01 | Phase 14 | Pending |
| VFX-02 | Phase 14 | Pending |
| VFX-03 | Phase 14 | Pending |
| VFX-04 | Phase 14 | Pending |
| VFX-05 | Phase 14 | Pending |
| VFX-06 | Phase 14 | Pending |
| VFX-07 | Phase 14 | Pending |
| VFX-08 | Phase 14 | Pending |
| LOOP-01 | Phase 15 | Pending |
| LOOP-02 | Phase 15 | Pending |
| LOOP-03 | Phase 15 | Pending |
| LOOP-04 | Phase 15 | Pending |
| AUDIO-01 | Phase 16 | Pending |
| AUDIO-02 | Phase 16 | Pending |
| AUDIO-03 | Phase 16 | Pending |
| AUDIO-04 | Phase 16 | Pending |
| AUDIO-05 | Phase 16 | Pending |
| STACK-01 | Phase 17 | Pending |
| STACK-02 | Phase 17 | Pending |
| STACK-03 | Phase 17 | Pending |
| STACK-04 | Phase 17 | Pending |
| MANA-01 | Phase 17 | Pending |
| MANA-02 | Phase 17 | Pending |
| MANA-03 | Phase 17 | Pending |
| COMBAT-01 | Phase 17 | Pending |
| COMBAT-02 | Phase 17 | Pending |
| COMBAT-03 | Phase 17 | Pending |
| COMBAT-04 | Phase 17 | Pending |

**Coverage:**
- v1.1 requirements: 46 total
- Mapped to phases: 46
- Unmapped: 0

---
*Requirements defined: 2026-03-08*
*Last updated: 2026-03-08 after roadmap creation*
