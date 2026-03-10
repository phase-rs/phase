# Phase 16: Audio System - Context

**Gathered:** 2026-03-09
**Status:** Ready for planning

<domain>
## Phase Boundary

Game events produce sound effects and background music plays during matches, all configurable by the player. SFX play on core gameplay events via Web Audio API using Forge's audio assets. Background music rotates through available tracks with crossfade. Independent volume controls with HUD quick-mute and PreferencesModal sliders. iOS/iPadOS AudioContext warm-up on first user interaction.

</domain>

<decisions>
## Implementation Decisions

### SFX-event mapping
- SFX synced with animation steps — sound fires at the same moment the visual effect plays, hooking into the existing event normalizer / animation step pipeline
- ~15 core gameplay events get sounds: DamageDealt, LifeChanged, SpellCast, CreatureDestroyed, AttackersDeclared, BlockersDeclared, LandPlayed, CardDrawn, SpellCountered, TokenCreated, GameStarted, GameOver, PermanentSacrificed, CounterAdded, AbilityActivated
- Skip bookkeeping events: PhaseChanged, PriorityPassed, PermanentTapped, PermanentUntapped, ManaAdded, DamageCleared, ZoneChanged, StackPushed, StackResolved, etc.
- SFX still play when animation speed is Instant (skip all animations) — audio fires rapidly without visual delay
- Simultaneous same-type events (e.g., board wipe killing 4 creatures) consolidate into a single, slightly louder sound — matches how the animation normalizer already groups events into steps

### Music behavior
- Rotate all available tracks regardless of deck colors — no WUBRG theming for now, keep it simple
- Simple crossfade (2-3 second overlap) between tracks using Web Audio gain nodes
- Auto-play when game starts if not muted — respects saved mute/volume preference
- First-time users hear music by default

### Asset sourcing & loading
- Bundle Forge's original SFX and music files in `public/audio/sfx/` and `public/audio/music/`
- M4A (AAC) format for all audio assets — universal browser support (Chrome, Firefox, Safari, Edge), excellent compression, no format fallback needed
- Eager preload all SFX into AudioBuffers during game initialization — ~39 small files, decode takes <1 second, zero latency on first play
- Music streams progressively (not preloaded) — files are larger, load on demand

### In-game controls
- HUD: small speaker icon for master mute toggle (mutes/unmutes both SFX and music together)
- PreferencesModal: independent SFX volume slider (0-100%) and Music volume slider (0-100%), each with its own mute toggle
- Default volumes: SFX 70%, Music 40% — SFX prominent for gameplay feedback, music ambient
- All audio preferences persist to localStorage via existing preferencesStore

### Claude's Discretion
- Exact SFX-to-asset mapping (which of Forge's 39 files maps to which event)
- AudioContext management and warm-up implementation details
- Game-over music behavior (fade out vs keep playing)
- Music track ordering and shuffle logic
- Web Audio node graph architecture
- HUD mute icon design and placement

</decisions>

<specifics>
## Specific Ideas

- User wants all tracks rotating for now rather than WUBRG-themed selection — simplicity over sophistication
- M4A chosen specifically to avoid OGG-on-Safari compatibility issues — single format, no fallbacks
- SFX should still play during Instant animation speed — "you hear the game progressing" even without visuals
- Consolidate simultaneous sounds to avoid cacophony on board wipes

</specifics>

<code_context>
## Existing Code Insights

### Reusable Assets
- `preferencesStore.ts`: Zustand + persist store — extend with `sfxVolume`, `musicVolume`, `sfxMuted`, `musicMuted`, `masterMuted` fields
- `AnimationOverlay.tsx` + event normalizer: Already processes GameEvents into animation steps — audio hooks into the same step pipeline
- `dispatch.ts`: Async dispatch pipeline with snapshot-before-dispatch — audio trigger point is during step playback
- 34 `GameEvent` types in `adapter/types.ts` (lines 217-250) — source for SFX trigger mapping

### Established Patterns
- Zustand stores with selector pattern for reactive subscriptions
- Event normalizer groups related events into animation steps (Phase 14 decision)
- `preferencesStore` persists to localStorage with `zustand/middleware/persist`
- Non-reactive reads via `getState()` for performance-sensitive code (ParticleCanvas pattern from Phase 14)

### Integration Points
- `preferencesStore`: Add audio preference fields alongside existing VFX quality and animation speed
- `PreferencesModal`: Add Audio section with SFX/music sliders and mute toggles
- `PlayerHud`: Add speaker icon for master mute toggle
- Animation step pipeline: Hook SFX playback into step execution alongside VFX
- `GamePage` or `GameProvider`: Initialize AudioContext and preload SFX on game mount

</code_context>

<deferred>
## Deferred Ideas

- WUBRG-themed track selection based on deck colors (AUDIO-03 simplified to track rotation for now — revisit when more music assets are available)
- Context-specific music for different screens (title, deck builder, game) — POLISH-02 in v2

</deferred>

---

*Phase: 16-audio-system*
*Context gathered: 2026-03-09*
