# Phase 16: Audio System - Research

**Researched:** 2026-03-09
**Domain:** Web Audio API, game audio pipeline, browser autoplay policy
**Confidence:** HIGH

## Summary

Phase 16 adds sound effects and background music to the game. The Web Audio API provides everything needed natively -- no third-party audio libraries are required. SFX are preloaded into AudioBuffers during game initialization (39 small MP3 files decode in under a second), while music streams via `<audio>` elements with Web Audio gain nodes for crossfading. The audio system hooks into the existing animation step pipeline in `dispatch.ts`, firing sounds at the same moment visual effects play.

The primary technical challenges are: (1) iOS/iPadOS AudioContext warm-up requiring user interaction before any audio plays, (2) consolidating simultaneous same-type events into a single louder sound, and (3) crossfading between music tracks using Web Audio gain nodes. All are well-documented browser API patterns with no library dependencies needed.

**Primary recommendation:** Build a standalone `AudioManager` class (not a React component) that owns the AudioContext, preloaded buffers, and music playback. Hook it into the dispatch pipeline alongside the animation system. Extend `preferencesStore` with audio preference fields.

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions
- SFX synced with animation steps -- sound fires at the same moment the visual effect plays, hooking into the existing event normalizer / animation step pipeline
- ~15 core gameplay events get sounds: DamageDealt, LifeChanged, SpellCast, CreatureDestroyed, AttackersDeclared, BlockersDeclared, LandPlayed, CardDrawn, SpellCountered, TokenCreated, GameStarted, GameOver, PermanentSacrificed, CounterAdded, AbilityActivated
- Skip bookkeeping events: PhaseChanged, PriorityPassed, PermanentTapped, PermanentUntapped, ManaAdded, DamageCleared, ZoneChanged, StackPushed, StackResolved, etc.
- SFX still play when animation speed is Instant (skip all animations) -- audio fires rapidly without visual delay
- Simultaneous same-type events consolidate into a single, slightly louder sound
- Rotate all available tracks regardless of deck colors -- no WUBRG theming for now
- Simple crossfade (2-3 second overlap) between tracks using Web Audio gain nodes
- Auto-play when game starts if not muted -- respects saved mute/volume preference
- First-time users hear music by default
- Bundle Forge's original SFX and music files in `public/audio/sfx/` and `public/audio/music/`
- M4A (AAC) format for all audio assets -- universal browser support, no format fallback needed
- Eager preload all SFX into AudioBuffers during game initialization
- Music streams progressively (not preloaded)
- HUD: small speaker icon for master mute toggle (mutes/unmutes both SFX and music together)
- PreferencesModal: independent SFX volume slider (0-100%) and Music volume slider (0-100%), each with its own mute toggle
- Default volumes: SFX 70%, Music 40%
- All audio preferences persist to localStorage via existing preferencesStore

### Claude's Discretion
- Exact SFX-to-asset mapping (which of Forge's 39 files maps to which event)
- AudioContext management and warm-up implementation details
- Game-over music behavior (fade out vs keep playing)
- Music track ordering and shuffle logic
- Web Audio node graph architecture
- HUD mute icon design and placement

### Deferred Ideas (OUT OF SCOPE)
- WUBRG-themed track selection based on deck colors (AUDIO-03 simplified to track rotation for now)
- Context-specific music for different screens (title, deck builder, game) -- POLISH-02 in v2
</user_constraints>

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|-----------------|
| AUDIO-01 | SFX play on game events using Forge's 39 sound effect assets via Web Audio API | AudioManager class preloads all 39 Forge SFX into AudioBuffers. Event-to-SFX mapping derived from Forge's EventVisualizer.java. Fires during dispatch step processing. |
| AUDIO-02 | Background music plays during matches using Forge's battle music tracks (CC-BY 3.0) | 4 match tracks from Forge (Kevin MacLeod, CC-BY 3.0). Stream via HTMLAudioElement connected to Web Audio gain nodes for volume control and crossfading. |
| AUDIO-03 | Music auto-selects WUBRG-themed tracks based on player's deck colors when available | Simplified per CONTEXT.md: rotate all tracks, no WUBRG theming. Sequential or shuffle rotation through 4 available tracks. |
| AUDIO-04 | Independent volume controls for SFX and music with mute toggles | preferencesStore extended with sfxVolume, musicVolume, sfxMuted, musicMuted, masterMuted. PreferencesModal gets Audio section with sliders. HUD gets speaker icon for master mute. |
| AUDIO-05 | iOS/iPadOS AudioContext warm-up on first user interaction | AudioContext created in suspended state, resumed on first click/tap/keydown via one-shot event listener. Standard browser autoplay policy pattern. |
</phase_requirements>

## Standard Stack

### Core
| Library | Version | Purpose | Why Standard |
|---------|---------|---------|--------------|
| Web Audio API | Native | SFX playback, gain control, crossfading | Built into all modern browsers. No library needed for this use case. |
| HTMLAudioElement | Native | Music streaming | Native browser API for streaming large audio files without full preload. |
| Zustand | 5.x (existing) | Audio preferences persistence | Already used for preferencesStore with persist middleware. |

### Supporting
| Library | Version | Purpose | When to Use |
|---------|---------|---------|-------------|
| N/A | - | - | No additional dependencies needed. Web Audio API covers all requirements. |

### Alternatives Considered
| Instead of | Could Use | Tradeoff |
|------------|-----------|----------|
| Raw Web Audio API | howler.js | Howler adds ~30KB for cross-browser normalization we don't need (M4A works everywhere). Adds unnecessary abstraction over simple AudioBuffer playback. |
| Raw Web Audio API | tone.js | Tone.js is ~150KB, designed for music synthesis. Massive overkill for playing pre-recorded SFX and streaming music. |
| HTMLAudioElement for music | AudioBuffer for music | AudioBuffer requires full file download before playback. Music files are large (several MB). HTMLAudioElement streams progressively. |

**Installation:**
```bash
# No new packages needed -- Web Audio API is native
```

## Architecture Patterns

### Recommended Project Structure
```
client/src/
├── audio/
│   ├── AudioManager.ts         # Singleton class: AudioContext, SFX buffers, music player
│   ├── sfxMap.ts               # Event type -> SFX file mapping
│   └── __tests__/
│       └── AudioManager.test.ts
├── stores/
│   └── preferencesStore.ts     # Extended with audio preferences
├── components/
│   ├── hud/
│   │   └── PlayerHud.tsx       # Extended with mute toggle icon
│   └── settings/
│       └── PreferencesModal.tsx # Extended with Audio section
└── game/
    └── dispatch.ts             # Extended to fire SFX during step processing
```

### Pattern 1: AudioManager Singleton (Not a React Component)
**What:** A plain TypeScript class that manages AudioContext lifecycle, SFX buffer preloading, SFX playback, and music streaming. Not a React component or hook -- accessed via module-level instance.
**When to use:** Always. Audio is a side-effect system that runs alongside React, not inside it.
**Why:** Matches the project's established pattern of module-level singletons for performance-sensitive systems (see `dispatch.ts` module-level mutex, `ScreenShake` as a plain function, `ParticleCanvas` reading `getState()` non-reactively).
**Example:**
```typescript
// Source: project pattern from dispatch.ts, ScreenShake.tsx
class AudioManager {
  private ctx: AudioContext | null = null;
  private sfxBuffers = new Map<string, AudioBuffer>();
  private sfxGain: GainNode | null = null;
  private musicGain: GainNode | null = null;
  private currentMusic: HTMLAudioElement | null = null;

  /** Create AudioContext on first user interaction */
  warmUp(): void {
    if (this.ctx) return;
    this.ctx = new AudioContext();
    this.sfxGain = this.ctx.createGain();
    this.sfxGain.connect(this.ctx.destination);
    this.musicGain = this.ctx.createGain();
    this.musicGain.connect(this.ctx.destination);

    // Apply saved preferences
    const prefs = usePreferencesStore.getState();
    this.sfxGain.gain.value = prefs.sfxMuted ? 0 : prefs.sfxVolume / 100;
    this.musicGain.gain.value = prefs.musicMuted ? 0 : prefs.musicVolume / 100;
  }

  /** Preload all SFX files into AudioBuffers */
  async preloadSfx(): Promise<void> {
    if (!this.ctx) return;
    const files = Object.values(SFX_MAP);
    const unique = [...new Set(files)];
    await Promise.all(unique.map(f => this.loadBuffer(f)));
  }

  /** Play a named SFX */
  playSfx(name: string, volume = 1.0): void {
    if (!this.ctx || !this.sfxGain) return;
    const buffer = this.sfxBuffers.get(name);
    if (!buffer) return;
    const source = this.ctx.createBufferSource();
    source.buffer = buffer;
    // Per-sound volume adjustment (for consolidation boost)
    if (volume !== 1.0) {
      const gain = this.ctx.createGain();
      gain.gain.value = volume;
      source.connect(gain);
      gain.connect(this.sfxGain);
    } else {
      source.connect(this.sfxGain);
    }
    source.start();
  }
}

export const audioManager = new AudioManager();
```

### Pattern 2: SFX Firing in Dispatch Pipeline
**What:** After normalizeEvents produces AnimationSteps, fire SFX for each step's effects. SFX fire regardless of animation speed setting.
**When to use:** Every dispatch cycle.
**Why:** SFX are synced with animation steps per locked decision. Even when animations are instant (multiplier=0), SFX still play.
**Example:**
```typescript
// In dispatch.ts processAction(), after step 4 (normalizeEvents):
// Fire SFX for all steps immediately when instant, or per-step during animation
function fireSfxForStep(step: AnimationStep): void {
  const eventTypes = step.effects.map(e => e.type);
  // Group same-type events for consolidation
  const typeCounts = new Map<string, number>();
  for (const type of eventTypes) {
    typeCounts.set(type, (typeCounts.get(type) ?? 0) + 1);
  }
  for (const [type, count] of typeCounts) {
    const sfxName = SFX_MAP[type];
    if (!sfxName) continue;
    // Consolidate: single sound, slightly boosted volume for multiples
    const volume = count > 1 ? Math.min(1.0 + count * 0.15, 1.5) : 1.0;
    audioManager.playSfx(sfxName, volume);
  }
}
```

### Pattern 3: Music Streaming via HTMLAudioElement + Web Audio
**What:** Use HTMLAudioElement for progressive streaming, pipe it through a MediaElementSourceNode into a GainNode for volume control and crossfading.
**When to use:** Background music playback.
**Why:** HTMLAudioElement handles streaming and buffering. Web Audio gain nodes provide precise volume control and crossfade scheduling.
**Example:**
```typescript
// Source: MDN Web Audio API best practices
startMusic(trackUrl: string): void {
  if (!this.ctx || !this.musicGain) return;
  const audio = new Audio(trackUrl);
  audio.loop = false; // We handle track rotation manually
  const source = this.ctx.createMediaElementSource(audio);
  source.connect(this.musicGain);
  audio.play();
  audio.addEventListener('ended', () => this.playNextTrack());
  this.currentMusic = audio;
}

crossfadeTo(nextTrackUrl: string, duration = 2.5): void {
  if (!this.ctx || !this.musicGain) return;
  const now = this.ctx.currentTime;
  // Fade out current
  this.musicGain.gain.linearRampToValueAtTime(0, now + duration);
  // After fade, swap tracks
  setTimeout(() => {
    this.currentMusic?.pause();
    this.startMusic(nextTrackUrl);
    // Fade in new
    const vol = usePreferencesStore.getState().musicVolume / 100;
    this.musicGain!.gain.setValueAtTime(0, this.ctx!.currentTime);
    this.musicGain!.gain.linearRampToValueAtTime(vol, this.ctx!.currentTime + duration);
  }, duration * 1000);
}
```

### Pattern 4: iOS AudioContext Warm-Up
**What:** Attach a one-shot event listener to the document that creates/resumes the AudioContext on first user interaction.
**When to use:** Application initialization.
**Why:** iOS Safari requires AudioContext to be created or resumed within a user gesture event handler. Without this, no audio will play.
**Example:**
```typescript
// Source: MDN Autoplay guide, Matt Montag's unlock pattern
function initAudioOnInteraction(): void {
  const handler = () => {
    audioManager.warmUp();
    if (audioManager.ctx?.state === 'suspended') {
      audioManager.ctx.resume();
    }
    document.removeEventListener('click', handler);
    document.removeEventListener('touchstart', handler);
    document.removeEventListener('keydown', handler);
  };
  document.addEventListener('click', handler);
  document.addEventListener('touchstart', handler);
  document.addEventListener('keydown', handler);
}
```

### Anti-Patterns to Avoid
- **Creating AudioContext on page load without user gesture:** Will be suspended on iOS/iPadOS and many desktop browsers. Always warm up on interaction.
- **Creating new AudioContext per sound:** Browsers limit the number of active contexts. Use one shared AudioContext for the entire application.
- **Preloading music into AudioBuffers:** Music files are several MB. Preloading wastes bandwidth and delays game start. Stream via HTMLAudioElement instead.
- **Using React state for audio playback tracking:** Audio is a fire-and-forget side effect. Putting playback state in React causes unnecessary re-renders. Use module-level state like the dispatch pipeline does.
- **Playing concurrent instances of the same SFX rapidly:** Board wipes with 4+ simultaneous CreatureDestroyed events should consolidate into one slightly louder sound, not 4 overlapping identical sounds.

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| Audio format compatibility | Format detection/fallback system | M4A (AAC) only | M4A has universal browser support (Chrome, Firefox, Safari, Edge). No fallback needed. |
| AudioContext lifecycle | Custom state machine | Browser's built-in state property + resume() | AudioContext.state is already "suspended", "running", or "closed". Just check and resume. |
| Volume persistence | Custom localStorage logic | Zustand persist (existing pattern) | preferencesStore already persists to localStorage. Just add fields. |
| Audio sprites for SFX | Sprite packing + offset seeking | Individual AudioBuffers | With only ~15 unique SFX files, individual buffers are simpler and have zero-latency playback. Sprites add complexity for minimal benefit at this scale. |

**Key insight:** The Web Audio API is purpose-built for game audio. At this scale (15 SFX, 4 music tracks), the raw API is cleaner than any abstraction library.

## Common Pitfalls

### Pitfall 1: AudioContext Created Before User Interaction
**What goes wrong:** AudioContext starts in "suspended" state on iOS/iPadOS. Audio never plays.
**Why it happens:** Code creates AudioContext during module initialization or React mount, before any user click/tap.
**How to avoid:** Lazy-initialize AudioContext in a user gesture handler. Use a one-shot event listener pattern.
**Warning signs:** `audioContext.state === "suspended"` after initialization.

### Pitfall 2: MediaElementSourceNode Cannot Be Reused
**What goes wrong:** Error when trying to create a second MediaElementSourceNode from the same HTMLAudioElement.
**Why it happens:** An HTMLAudioElement can only be connected to one MediaElementSourceNode. Once connected, calling `createMediaElementSource` again on the same element throws.
**How to avoid:** Create a new HTMLAudioElement for each track. Store the MediaElementSourceNode alongside the element.
**Warning signs:** `InvalidStateError` when creating MediaElementSourceNode.

### Pitfall 3: Gain Scheduling Conflicts
**What goes wrong:** Setting `gain.value` directly after using `linearRampToValueAtTime` has no effect.
**Why it happens:** Scheduled automation takes priority over direct value assignments.
**How to avoid:** Call `gain.cancelScheduledValues(currentTime)` before setting gain.value directly. Use `setValueAtTime()` as the anchor before any ramp.
**Warning signs:** Volume controls appear unresponsive after crossfade.

### Pitfall 4: Overlapping Sounds on Board Wipes
**What goes wrong:** 4+ CreatureDestroyed sounds play simultaneously, creating cacophonous audio.
**Why it happens:** The animation normalizer groups same-type events into one step, but each effect in the step fires its own sound.
**How to avoid:** Per the locked decision, consolidate same-type events within a step into a single sound with a small volume boost.
**Warning signs:** Audio clipping or distortion during combat resolution.

### Pitfall 5: Music Not Playing on Game Start (First Visit)
**What goes wrong:** Music set to auto-play but doesn't start because AudioContext isn't warmed up yet.
**Why it happens:** Game starts before the user has interacted with the page.
**How to avoid:** The user necessarily clicks "Play" or selects a deck before reaching the game page. Wire AudioContext warm-up to these early interactions. By the time GamePage mounts, AudioContext should already be running.
**Warning signs:** Music only starts after the first in-game click.

### Pitfall 6: iOS Ringer Silent Mode
**What goes wrong:** No audio at all on iOS, even after AudioContext warm-up.
**Why it happens:** iOS respects the hardware ringer switch. When set to silent/vibrate, Web Audio API is muted.
**How to avoid:** This is by design and cannot be overridden. No action needed -- it's expected iOS behavior. Do not treat this as a bug.
**Warning signs:** Audio works in Safari on macOS but not on iOS with ringer off.

## Code Examples

### Forge SFX Asset Inventory (39 files)
```
Source: GitHub API - Card-Forge/forge/forge-gui/res/sound/

add_counter.mp3    artifact.mp3         artifact_creature.mp3
black_land.mp3     block.mp3            blue_land.mp3
button_press.mp3   coins_drop.mp3       creature.mp3
daytime.mp3        destroy.mp3          discard.mp3
draw.mp3           enchant.mp3          end_of_turn.mp3
equip.mp3          flip_card.mp3        flip_coin.mp3
green_land.mp3     instant.mp3          life_loss.mp3
lose_duel.mp3      mana_burn.mp3        nighttime.mp3
planeswalker.mp3   poison.mp3           red_land.mp3
rewind.mp3         roll_die.mp3         shuffle.mp3
sorcery.mp3        speedup.mp3          sprocket.mp3
take_shard.mp3     tap.mp3              token.mp3
untap.mp3          white_land.mp3       win_duel.mp3
```

### Forge Music Asset Inventory (4 match tracks)
```
Source: GitHub API - Card-Forge/forge/forge-gui/res/music/match/

Dangerous.mp3
Failing Defense.mp3
Hitman.mp3
Prelude and Action.mp3

License: CC-BY 3.0 - Kevin MacLeod (incompetech.com)
```

### Recommended Event-to-SFX Mapping
```typescript
// Source: Derived from Forge's EventVisualizer.java + SoundEffectType.java
// Maps the 15 core GameEvent types to Forge SFX files

export const SFX_MAP: Record<string, string> = {
  // Core gameplay events
  DamageDealt:         'destroy',      // Forge uses 'damage' but file doesn't exist; destroy is closest
  LifeChanged:         'life_loss',    // Covers both damage and gain (same audio cue for feedback)
  SpellCast:           'instant',      // Generic spell sound; could switch on card type if desired
  CreatureDestroyed:   'destroy',
  AttackersDeclared:   'creature',     // Creature sound for attack declaration
  BlockersDeclared:    'block',
  LandPlayed:          'green_land',   // Default land sound; could match color but keeping simple
  CardDrawn:           'draw',
  SpellCountered:      'sorcery',      // Distinct from regular spell cast
  TokenCreated:        'token',
  GameStarted:         'shuffle',      // Shuffling at game start (Forge uses start_duel but file doesn't exist)
  GameOver:            'win_duel',     // For player wins; lose_duel for losses
  PermanentSacrificed: 'destroy',      // Forge uses 'sacrifice' but file doesn't exist; destroy is closest
  CounterAdded:        'add_counter',
  AbilityActivated:    'enchant',      // Enchantment sound for activated abilities
};

// Note: Some Forge SoundEffectType entries reference files that don't exist
// in the actual res/sound/ directory (damage.mp3, sacrifice.mp3, start_duel.mp3,
// exile.mp3, life_gain.mp3, etc.). The mapping above uses only files confirmed
// to exist in the directory listing.
```

### Preloading SFX AudioBuffers
```typescript
// Source: MDN Web Audio API, MDN Audio for Web Games
private async loadBuffer(filename: string): Promise<void> {
  if (!this.ctx) return;
  try {
    const response = await fetch(`/audio/sfx/${filename}.m4a`);
    const arrayBuffer = await response.arrayBuffer();
    const audioBuffer = await this.ctx.decodeAudioData(arrayBuffer);
    this.sfxBuffers.set(filename, audioBuffer);
  } catch (err) {
    console.warn(`Failed to load SFX: ${filename}`, err);
  }
}
```

### PreferencesStore Audio Extension
```typescript
// Source: existing preferencesStore.ts pattern
interface PreferencesState {
  // ... existing fields ...
  sfxVolume: number;        // 0-100, default 70
  musicVolume: number;      // 0-100, default 40
  sfxMuted: boolean;        // default false
  musicMuted: boolean;      // default false
  masterMuted: boolean;     // default false
}
```

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| `new Audio().play()` for SFX | Web Audio API AudioBuffer + BufferSourceNode | 2015+ | Zero-latency playback, precise timing, gain control |
| Flash for audio in browsers | Web Audio API | 2017 (Flash EOL) | Native API, no plugins |
| Auto-play audio on page load | AudioContext.resume() on user gesture | 2018+ (Chrome 66, Safari 12) | Must design for user interaction first |
| OGG + MP3 format fallback | M4A (AAC) single format | Safari 11+ (2017) | AAC universally supported, no fallback needed |
| howler.js for cross-browser audio | Raw Web Audio API | Ongoing | Raw API sufficient for simple use cases; howler.js still useful for complex scenarios |

**Deprecated/outdated:**
- `webkitAudioContext` prefix: No longer needed. `AudioContext` is unprefixed in all modern browsers.
- `createGainNode()`: Deprecated in favor of `createGain()`.
- OGG-only audio: Safari doesn't support OGG Vorbis. M4A (AAC) is the universal format.

## Open Questions

1. **Asset format conversion: MP3 to M4A**
   - What we know: Forge's assets are MP3. User decision requires M4A (AAC).
   - What's unclear: Whether to convert during build or pre-convert and commit.
   - Recommendation: Pre-convert all assets to M4A using ffmpeg and commit to `public/audio/`. This avoids build complexity and ensures assets are ready. MP3 originals are not checked in.

2. **Game-over music behavior**
   - What we know: Claude's discretion per CONTEXT.md.
   - What's unclear: Should music fade out on game over, stop abruptly, or keep playing?
   - Recommendation: Fade out over 2 seconds when GameOver event fires. Clean ending.

3. **Music shuffle vs sequential**
   - What we know: Claude's discretion. 4 tracks available.
   - What's unclear: Sequential rotation or random shuffle?
   - Recommendation: Shuffle with no-repeat (Fisher-Yates shuffle of track list, play through, re-shuffle). Avoids the same track always following another.

4. **SFX files that don't exist in Forge's repo**
   - What we know: Forge's SoundEffectType.java references ~65 enum values, but only 39 MP3 files exist in res/sound/. Several referenced files (damage.mp3, sacrifice.mp3, start_duel.mp3, exile.mp3, life_gain.mp3, etc.) are missing.
   - What's unclear: Whether these files exist elsewhere or were never created.
   - Recommendation: Map only to confirmed-existing files. Use closest alternative sounds for missing assets (e.g., destroy.mp3 for sacrifice).

## Validation Architecture

### Test Framework
| Property | Value |
|----------|-------|
| Framework | Vitest 3.x + jsdom |
| Config file | `client/vitest.config.ts` |
| Quick run command | `cd client && pnpm test -- --run --reporter=verbose` |
| Full suite command | `cd client && pnpm test -- --run` |

### Phase Requirements to Test Map
| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|-------------|
| AUDIO-01 | AudioManager.playSfx fires correct SFX for each event type | unit | `cd client && pnpm test -- --run src/audio/__tests__/AudioManager.test.ts` | Wave 0 |
| AUDIO-01 | SFX consolidation: multiple same-type events produce single sound | unit | `cd client && pnpm test -- --run src/audio/__tests__/AudioManager.test.ts` | Wave 0 |
| AUDIO-02 | Music starts on game start, crossfades between tracks | unit | `cd client && pnpm test -- --run src/audio/__tests__/AudioManager.test.ts` | Wave 0 |
| AUDIO-03 | Track rotation cycles through all available tracks | unit | `cd client && pnpm test -- --run src/audio/__tests__/AudioManager.test.ts` | Wave 0 |
| AUDIO-04 | preferencesStore audio fields persist and hydrate correctly | unit | `cd client && pnpm test -- --run src/stores/__tests__/preferencesStore.test.ts` | Existing (extend) |
| AUDIO-04 | Volume/mute changes update gain nodes in real time | unit | `cd client && pnpm test -- --run src/audio/__tests__/AudioManager.test.ts` | Wave 0 |
| AUDIO-05 | AudioContext warm-up on user interaction | unit | `cd client && pnpm test -- --run src/audio/__tests__/AudioManager.test.ts` | Wave 0 |

### Sampling Rate
- **Per task commit:** `cd client && pnpm test -- --run --reporter=verbose`
- **Per wave merge:** `cd client && pnpm test -- --run`
- **Phase gate:** Full suite green before `/gsd:verify-work`

### Wave 0 Gaps
- [ ] `client/src/audio/__tests__/AudioManager.test.ts` -- covers AUDIO-01, AUDIO-02, AUDIO-03, AUDIO-05
- [ ] AudioContext mock in test-setup or test file (jsdom doesn't provide AudioContext)
- [ ] Extend `client/src/stores/__tests__/preferencesStore.test.ts` for audio preference fields

## Sources

### Primary (HIGH confidence)
- [MDN Web Audio API](https://developer.mozilla.org/en-US/docs/Web/API/Web_Audio_API) - AudioContext, AudioBuffer, GainNode, decodeAudioData APIs
- [MDN Web Audio API Best Practices](https://developer.mozilla.org/en-US/docs/Web/API/Web_Audio_API/Best_practices) - AudioContext creation, gain scheduling, autoplay policy
- [MDN Audio for Web Games](https://developer.mozilla.org/en-US/docs/Games/Techniques/Audio_for_Web_Games) - Game audio patterns, iOS unlock, preloading, crossfading
- [MDN Autoplay Guide](https://developer.mozilla.org/en-US/docs/Web/Media/Guides/Autoplay) - Browser autoplay restrictions and user gesture requirements
- [MDN AudioParam.linearRampToValueAtTime](https://developer.mozilla.org/en-US/docs/Web/API/AudioParam/linearRampToValueAtTime) - Gain ramp scheduling for crossfades
- GitHub API: Card-Forge/forge - SoundEffectType.java, EventVisualizer.java, res/sound/ directory listing, res/music/ directory listing (verified via gh api)

### Secondary (MEDIUM confidence)
- [Matt Montag - Unlock Web Audio in Safari](https://www.mattmontag.com/web/unlock-web-audio-in-safari-for-ios-and-macos) - iOS AudioContext unlock pattern details
- [Matt Harrison - Perfect Web Audio on iOS](https://matt-harrison.com/posts/web-audio/) - iOS-specific AudioContext warm-up strategies
- [web.dev - Getting Started with Web Audio API](https://web.dev/articles/webaudio-intro) - AudioBuffer loading patterns
- [Boris Smus - Web Audio API Book](https://webaudioapi.com/book/Web_Audio_API_Boris_Smus_html/ch02.html) - Crossfading techniques with gain nodes

### Tertiary (LOW confidence)
- None. All findings verified against official documentation or source code.

## Metadata

**Confidence breakdown:**
- Standard stack: HIGH - Web Audio API is a mature, well-documented browser API. No third-party libraries needed.
- Architecture: HIGH - AudioManager singleton pattern matches project conventions (dispatch.ts, ScreenShake). Integration points clearly identified in existing code.
- Pitfalls: HIGH - iOS AudioContext restrictions and gain scheduling conflicts are extensively documented on MDN and in community resources.
- Asset mapping: MEDIUM - Derived from Forge's Java source code (EventVisualizer.java). Some SFX files referenced in code don't exist in the repo. Mapping uses confirmed-existing files only.

**Research date:** 2026-03-09
**Valid until:** 2026-04-09 (stable -- Web Audio API is mature and changes slowly)
