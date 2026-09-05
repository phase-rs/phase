import type { StepEffect } from "../animation/types";
import { usePreferencesStore } from "../stores/preferencesStore";

import { fetchWithCache } from "./audioCache";
import { PLANESWALKER_THEME } from "./planeswalkerTheme";
import { findManifest, resolveTheme } from "./themeRegistry";
import type {
  AudioContextName,
  AudioThemeManifest,
  GamePhaseTag,
  ResolvedTheme,
  ThemeTrack,
} from "./types";

/** Fisher-Yates shuffle (in-place). */
function shuffle<T>(arr: T[]): T[] {
  for (let i = arr.length - 1; i > 0; i--) {
    const j = Math.floor(Math.random() * (i + 1));
    [arr[i], arr[j]] = [arr[j], arr[i]];
  }
  return arr;
}

const DEFAULT_PHASE_BREAKPOINTS = { mid: 5, late: 10 };

/**
 * How much of the active theme's SFX is playable — what the buffer map holds,
 * which is the same thing `playSfx` reads.
 *
 * - `skipped` — nothing to say: audio is off, there is no context, or the theme
 *   declares no SFX. Not a statement about whether the host can play sound.
 * - `none` — nothing decoded. On a host with a working media pipeline that does
 *   not happen, so it is the pipeline reporting itself broken.
 * - `partial` / `loaded` — some or all decoded; sound works either way.
 */
export type SfxPreloadResult = "skipped" | "none" | "partial" | "loaded";

class AudioManager {
  private ctx: AudioContext | null = null;
  private sfxBuffers = new Map<string, AudioBuffer>();
  private sfxGain: GainNode | null = null;
  private musicGain: GainNode | null = null;
  private currentAudio: HTMLAudioElement | null = null;
  private trackOrder: ThemeTrack[] = [];
  private trackIndex = 0;
  private isWarmedUp = false;
  private disabled = false;
  private deviceOpenArmed = false;
  private crossfadeInProgress = false;
  /** Incremented on every context/theme change to invalidate stale timeouts. */
  private generation = 0;
  /** Separate element for victory/defeat stingers, so setContext can stop them. */
  private stingerAudio: HTMLAudioElement | null = null;

  // Theme & context state
  private activeTheme: ResolvedTheme = resolveTheme(PLANESWALKER_THEME);
  private activeContext: AudioContextName = "menu";
  private battlefieldPhase: GamePhaseTag = "early";

  /**
   * Permanently skip audio for this session: `warmUp()` becomes a no-op so no
   * device-open path can run (every other device touch already guards on a
   * null `ctx`). Boot sets this for either fault it can detect — the shell
   * reporting the OS audio server wedged (WebKitGTK opens the device
   * synchronously on the page main thread, so opening it then would freeze
   * the page), or the platform's media pipeline failing to decode any sound
   * within the boot deadline, which leaves every later pipeline just as dead.
   * The second case latches after `warmUp()` has already built a context, so
   * `disable()` does not itself imply a null `ctx`; the invariant is that once
   * disabled, `ctx` stays null after any `dispose()`/`restart()` cycle, and an
   * `isWarmedUp`-based fast path must never bypass the `disabled` check.
   *
   * Because the context can outlive the latch — and can outlive it with
   * buffers already decoded — the flag guards every method that OPENS a media
   * pipeline, FEEDS one, or RESUMES one: `warmUp()` (the device),
   * `preloadSfx()` (a decode), `playTrack()`/`playStinger()` (a media element
   * source), `playSfx()` (a buffer source), `startMusic()` (the rotation entry
   * point), and `ensurePlayback()` (a direct `ctx.resume()`). A new method in any of those categories must join the
   * set. Pure parameter automation on nodes that already exist — gain ramps,
   * `dispose()`'s teardown — is inert on a dead context and stays unguarded.
   *
   * `setContext()` is deliberately NOT guarded: it is the teardown path as well
   * as the start path, so an early return would strand music that was already
   * playing when the latch fired, leaving a "disabled" manager audible. It
   * reaches playback only through `startMusic()`/`playTrack()`, both of which
   * check, so the pipeline stays shut while its `stopMusic()` half keeps
   * working.
   *
   * Asynchronous continuations RECHECK rather than inheriting their caller's
   * guard: an entry check is stale by the time a callback runs, so
   * `playTrack()`'s `ended` handler and its `play()`-rejection path each latch
   * on `disabled` and the generation before touching the context again, and
   * `loadBuffer()` rechecks `disabled` plus the captured context and theme
   * across both of its awaits. Every `await` and every callback in this file
   * that afterwards touches the context is covered by that rule; a new one
   * must recheck too, choosing the invalidation signal that matches what it
   * produces (generation for music rotation, theme identity for SFX buffers).
   */
  disable(): void {
    this.disabled = true;
  }

  get isDisabled(): boolean {
    return this.disabled;
  }

  /**
   * Allow device-open. Until the boot gate has a verdict, every warmUp()
   * caller — gesture handlers, VolumeControl restart/ensurePlayback, future
   * callers — is a no-op, so no UI path (e.g. Tab+Enter through the splash
   * overlay, which blocks pointers but is not a focus trap) can open the
   * device before audioDeviceSafe() resolves. ensurePreload() arms this
   * right after the verdict, on every platform.
   */
  armDeviceOpen(): void {
    this.deviceOpenArmed = true;
  }

  /** Create AudioContext and gain nodes. Apply saved volume preferences. */
  warmUp(): void {
    if (this.disabled || !this.deviceOpenArmed || this.isWarmedUp) return;
    this.ctx = new AudioContext();
    this.sfxGain = this.ctx.createGain();
    this.sfxGain.connect(this.ctx.destination);
    this.musicGain = this.ctx.createGain();
    this.musicGain.connect(this.ctx.destination);

    this.applySavedGains();
    this.isWarmedUp = true;
  }

  // ---------------------------------------------------------------------------
  // Theme loading
  // ---------------------------------------------------------------------------

  /**
   * Load a theme manifest: resolve it, clear old SFX buffers, and begin
   * background preload of SFX assets (does not block on external fetches).
   */
  async loadTheme(manifest: AudioThemeManifest): Promise<void> {
    this.activeTheme = resolveTheme(manifest);
    this.sfxBuffers.clear();
    // Fire background preload — do not await
    this.preloadSfx();
    // Restart music with the new theme's tracks if currently playing
    if (this.currentAudio) {
      this.setContext(this.activeContext, true);
    }
  }

  // ---------------------------------------------------------------------------
  // SFX
  // ---------------------------------------------------------------------------

  /**
   * How much of the active theme is playable right now.
   *
   * Reads the buffer map rather than the outcome of a preload pass, because
   * the map is what `playSfx` reads and because a caller may need an answer
   * while a pass is still in flight: `preloadSfx` cannot resolve until every
   * file settles, but the files that already decoded are playable the moment
   * they land. The boot deadline depends on that distinction — one file that
   * never settles must not cost the user every other sound (issue #6744).
   *
   * Sharing one classifier with `preloadSfx` is what keeps "what boot decided"
   * and "what is actually playable" from drifting apart.
   */
  sfxAvailability(): SfxPreloadResult {
    if (this.disabled || !this.ctx) return "skipped";
    const entries = Object.entries(this.activeTheme.sfxMap);
    const urls = [...new Set(entries.map(([, url]) => url))];
    if (urls.length === 0) return "skipped";

    const loaded = urls.filter((url) =>
      entries.some(([eventType, u]) => u === url && this.sfxBuffers.has(eventType)),
    ).length;
    if (loaded === 0) return "none";
    return loaded === urls.length ? "loaded" : "partial";
  }

  /**
   * Preload all unique SFX files into AudioBuffers (background, non-blocking).
   *
   * `loadBuffer` swallows per-file failures on purpose — one unreachable sound
   * must not take the rest down — so the result is read off the buffer map
   * afterwards. A pass where *nothing* decoded is not a partial failure; it is
   * the platform telling us it cannot play sound at all, and the boot gate has
   * to be able to tell those apart.
   */
  async preloadSfx(): Promise<SfxPreloadResult> {
    if (this.disabled || !this.ctx) return "skipped";
    const entries = Object.entries(this.activeTheme.sfxMap);
    const urls = [...new Set(entries.map(([, url]) => url))];
    if (urls.length === 0) return "skipped";

    await Promise.all(
      urls.map((url) => {
        // Find the eventType(s) that map to this URL
        const eventTypes = entries
          .filter(([_, u]) => u === url)
          .map(([et]) => et);
        return this.loadBuffer(url, eventTypes);
      }),
    );

    return this.sfxAvailability();
  }

  /** Play a single SFX by GameEvent type. */
  playSfx(eventType: string, volume = 1.0): void {
    // `disabled` can latch after warm-up with buffers already decoded, so the
    // ctx/gain guards below do not cover it: starting a source node on a
    // latched-off host would push audio at a stack we just declared dead.
    if (this.disabled || !this.ctx || !this.sfxGain) return;

    const buffer = this.sfxBuffers.get(eventType);
    if (!buffer) {
      console.debug(`[SFX] No buffer for "${eventType}" (loaded: ${[...this.sfxBuffers.keys()].join(", ")})`);
      return;
    }

    const prefs = usePreferencesStore.getState();
    if (this.computeEffectiveSfxGain(prefs) <= 0) return;

    const source = this.ctx.createBufferSource();
    source.buffer = buffer;

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

  /**
   * Play SFX for an animation step, consolidating same-type effects
   * into a single slightly louder sound.
   */
  playSfxForStep(effects: StepEffect[]): void {
    const typeCounts = new Map<string, number>();
    for (const effect of effects) {
      if (effect.displayOnly) continue;
      const sfxKey = this.resolveSfxKey(effect.event);
      typeCounts.set(sfxKey, (typeCounts.get(sfxKey) ?? 0) + 1);
    }

    for (const [type, count] of typeCounts) {
      if (!this.activeTheme.sfxMap[type]) continue;
      const volume =
        count > 1 ? Math.min(1.0 + count * 0.15, 1.5) : 1.0;
      this.playSfx(type, volume);
    }
  }

  // ---------------------------------------------------------------------------
  // Context management
  // ---------------------------------------------------------------------------

  /**
   * Switch audio context (e.g., "menu" → "battlefield").
   * If `force` is true, restarts music even if the context hasn't changed
   * (used by ensurePlayback and reconnection).
   */
  setContext(context: AudioContextName, force = false): void {
    if (context === this.activeContext && !force) {
      // Same context — only restart if music isn't playing
      if (this.currentAudio) return;
    }

    this.activeContext = context;
    this.generation++;
    this.stopStinger();

    if (this.currentAudio) {
      // Crossfade: fade out old track, then fade in new track after overlap
      const fadeDuration = 1.5;
      const gen = this.generation;
      this.stopMusic(fadeDuration);
      setTimeout(() => {
        // Bail if context changed again during fade
        if (this.generation !== gen) return;
        this.resetMusicGain();
        this.fadeInMusic();
      }, fadeDuration * 500); // Start new track at 50% through fade-out for overlap
    } else {
      this.resetMusicGain();
      this.startMusic();
    }
  }

  /**
   * Update the battlefield music phase. Triggers a track switch only when
   * the phase actually changes and the theme has phase-tagged tracks.
   */
  setBattlefieldPhase(phase: GamePhaseTag): void {
    if (phase === this.battlefieldPhase) return;

    // Record the phase immediately so it's never lost, even if a fade is
    // already in flight. nextTrackIndex re-filters against battlefieldPhase,
    // so the next natural rotation will pick up the new phase's tracks.
    this.battlefieldPhase = phase;

    if (this.crossfadeInProgress) return;

    // Only rebuild track list if we're currently in battlefield context
    if (this.activeContext !== "battlefield") return;

    // Check if the current track still matches the new phase
    const currentTrack = this.trackOrder[this.trackIndex];
    if (currentTrack && (currentTrack.phase === "any" || currentTrack.phase === phase)) {
      return; // Current track is fine for the new phase
    }

    // Rebuild and restart with phase-appropriate tracks
    if (this.currentAudio) {
      this.crossfadeInProgress = true;
      this.generation++;
      const gen = this.generation;
      this.stopMusic(2.5);
      setTimeout(() => {
        this.crossfadeInProgress = false;
        // Another generation-bumping call (setContext, playStinger, etc.)
        // interrupted us — restore gain so music isn't left silenced, then
        // let the interrupting caller drive playback.
        if (this.generation !== gen) {
          this.resetMusicGain();
          return;
        }
        this.resetMusicGain();
        this.startMusic();
      }, 2500);
    }
  }

  /** Get the phase breakpoints from the active theme (or defaults). */
  getPhaseBreakpoints(): { mid: number; late: number } {
    return (
      this.activeTheme.manifest.phaseBreakpoints ?? DEFAULT_PHASE_BREAKPOINTS
    );
  }

  // ---------------------------------------------------------------------------
  // Stingers
  // ---------------------------------------------------------------------------

  /**
   * Play a one-shot victory or defeat stinger. Uses a separate Audio element
   * to avoid triggering the track rotation `ended` handler.
   * Falls back to stopMusic(2.0) if the theme has no stinger tracks.
   */
  playStinger(context: "victory" | "defeat"): void {
    const tracks = this.activeTheme.musicByContext[context];
    if (tracks.length === 0) {
      this.stopMusic(2.0);
      return;
    }

    // Invalidate any in-flight crossfade/ended timeouts before stopping music
    this.generation++;
    // Stop current music immediately
    this.stopMusic(0);

    if (this.disabled || !this.ctx || !this.musicGain) return;

    // Reset music gain — cancelScheduledValues first so .value assignment
    // takes effect (WebAudio spec: automation overrides direct .value writes)
    const now = this.ctx.currentTime;
    this.musicGain.gain.cancelScheduledValues(now);
    const prefs = usePreferencesStore.getState();
    this.musicGain.gain.setValueAtTime(
      this.computeEffectiveMusicGain(prefs),
      now,
    );

    // Stop any previously playing stinger
    this.stopStinger();

    // Play stinger on a separate Audio element — NOT stored in this.currentAudio
    const track = tracks[Math.floor(Math.random() * tracks.length)];
    const audio = new Audio(track.url);
    audio.crossOrigin = "anonymous";
    const source = this.ctx.createMediaElementSource(audio);
    source.connect(this.musicGain);

    this.stingerAudio = audio;
    audio.addEventListener("ended", () => {
      if (this.stingerAudio === audio) this.stingerAudio = null;
    });

    audio.play().catch(() => {
      /* stinger playback failed — silent fallback */
    });
  }

  // ---------------------------------------------------------------------------
  // Music playback
  // ---------------------------------------------------------------------------

  /** Start music playback with shuffled track rotation for the active context. */
  startMusic(): void {
    if (this.disabled || !this.ctx || !this.musicGain) return;

    const prefs = usePreferencesStore.getState();
    if (prefs.musicMuted || prefs.masterMuted) return;

    let tracks = this.activeTheme.musicByContext[this.activeContext];

    // For battlefield context, filter by current phase
    if (this.activeContext === "battlefield") {
      const phaseFiltered = tracks.filter(
        (t) => t.phase === "any" || t.phase === this.battlefieldPhase,
      );
      if (phaseFiltered.length > 0) {
        tracks = phaseFiltered;
      }
      // If no tracks match the phase, use all tracks as fallback
    }

    if (tracks.length === 0) return;

    this.trackOrder = shuffle([...tracks]);
    this.trackIndex = 0;
    this.playTrack();
  }

  /** Start music with a fade-in from silence. */
  fadeInMusic(duration = 1.5): void {
    if (!this.ctx || !this.musicGain) return;

    const prefs = usePreferencesStore.getState();
    const targetVolume = this.computeEffectiveMusicGain(prefs);

    // Start from silence
    const now = this.ctx.currentTime;
    this.musicGain.gain.cancelScheduledValues(now);
    this.musicGain.gain.setValueAtTime(0, now);
    this.musicGain.gain.linearRampToValueAtTime(targetVolume, now + duration);

    this.startMusic();
  }

  /** Stop music with optional fade-out. */
  stopMusic(fadeOut = 2.0): void {
    if (!this.ctx || !this.musicGain || !this.currentAudio) return;

    const audio = this.currentAudio;
    this.currentAudio = null;

    if (fadeOut <= 0) {
      // Immediate stop — no deferred pause that can race with new playback
      audio.pause();
    } else {
      const now = this.ctx.currentTime;
      this.musicGain.gain.cancelScheduledValues(now);
      this.musicGain.gain.setValueAtTime(this.musicGain.gain.value, now);
      this.musicGain.gain.linearRampToValueAtTime(0, now + fadeOut);

      setTimeout(() => {
        audio.pause();
      }, fadeOut * 1000);
    }
  }

  /**
   * Resume audio playback after a user gesture (e.g. unmute button click).
   * Warms up the AudioContext if needed, resumes it if suspended,
   * and ensures music is playing for the current context.
   *
   * Returns early while disabled rather than relying on its callees' guards:
   * `resume()` below acts on `ctx` directly, and the boot deadline can latch
   * with that context still live, so a gesture handler would otherwise walk
   * straight back into the media path boot just declared dead.
   */
  ensurePlayback(): void {
    if (this.disabled) return;
    this.warmUp();
    this.preloadSfx();

    if (this.ctx?.state === "suspended") {
      this.ctx.resume();
    }

    if (!this.currentAudio) {
      this.setContext(this.activeContext, true);
    }
  }

  /** Read current preferences and update gain node values. */
  updateVolumes(): void {
    if (!this.sfxGain || !this.musicGain || !this.ctx) return;

    const now = this.ctx.currentTime;

    this.sfxGain.gain.cancelScheduledValues(now);
    this.sfxGain.gain.setValueAtTime(this.sfxGain.gain.value, now);

    this.musicGain.gain.cancelScheduledValues(now);
    this.musicGain.gain.setValueAtTime(this.musicGain.gain.value, now);

    this.applySavedGains();
  }

  /** Stop music, close AudioContext. */
  dispose(): void {
    this.generation++;
    this.crossfadeInProgress = false;
    this.stopStinger();
    if (this.currentAudio) {
      this.currentAudio.pause();
      this.currentAudio = null;
    }
    if (this.ctx) {
      this.ctx.close();
      this.ctx = null;
    }
    this.sfxGain = null;
    this.musicGain = null;
    this.sfxBuffers.clear();
    this.isWarmedUp = false;
  }

  /**
   * Tear down and fully rebuild the AudioContext, reload the theme,
   * and restart playback. Use this to recover from iOS/iPadOS audio
   * suspension where resume() alone doesn't work.
   */
  async restart(): Promise<void> {
    const context = this.activeContext;
    const phase = this.battlefieldPhase;
    this.dispose();
    this.warmUp();
    try {
      const prefs = usePreferencesStore.getState();
      const manifest = await findManifest(
        prefs.audioThemeId,
        prefs.customThemeUrls,
      );
      await this.loadTheme(manifest);
    } catch {
      await this.loadTheme(PLANESWALKER_THEME);
    }
    this.activeContext = context;
    this.battlefieldPhase = phase;
    this.setContext(context, true);
  }

  /** Return a human-readable diagnostic string for the debug panel. */
  diagnostics(): string {
    const ctxState = this.disabled ? "disabled" : (this.ctx?.state ?? "none");
    const playing = this.currentAudio ? !this.currentAudio.paused : false;
    return `ctx=${ctxState} music=${playing ? "playing" : "stopped"} context=${this.activeContext}`;
  }

  // ---------------------------------------------------------------------------
  // Private helpers
  // ---------------------------------------------------------------------------

  private stopStinger(): void {
    if (this.stingerAudio) {
      this.stingerAudio.pause();
      this.stingerAudio = null;
    }
  }

  /**
   * Map a GameEvent to an SFX key. Splits LifeChanged into LifeGained/LifeLost
   * so the theme can assign distinct sounds for healing vs damage.
   */
  private resolveSfxKey(event: { type: string; data?: unknown }): string {
    if (event.type === "GroupedDamageFlurry") return "DamageDealt";
    if (event.type === "LifeChanged") {
      const data = event.data as { amount: number } | undefined;
      if (data && data.amount > 0) return "LifeGained";
      return "LifeLost";
    }
    return event.type;
  }

  /** Decode one file into the buffer map. Failures are logged, not thrown —
   *  `sfxAvailability()` reads the map to find out what survived.
   *
   *  Bytes are in flight across two awaits, so the entry guard is stale twice
   *  over: `disable()` can latch, `dispose()`/`restart()` can swap the context,
   *  and `loadTheme()` can clear the buffer map for a different theme, all
   *  while this load is waiting. The context and theme are captured up front
   *  and rechecked before opening the decode and again before committing, so a
   *  slow fetch cannot reopen a pipeline boot declared unusable, decode against
   *  a replaced context, or repopulate a map that was cleared for another
   *  theme.
   *
   *  Deliberately NOT gated on `generation`: that tracks music-rotation
   *  identity and is bumped by `setContext`/`playStinger`/`setBattlefieldPhase`,
   *  so gating SFX on it would throw away good buffers every time the player
   *  moves between menu and battlefield. `loadTheme()` conversely may not bump
   *  it at all, so it does not even capture the invalidation that matters here.
   */
  private async loadBuffer(url: string, eventTypes: string[]): Promise<void> {
    const ctx = this.ctx;
    const theme = this.activeTheme;
    if (this.disabled || !ctx) return;
    /** Whether this load still belongs to the audio stack it started on. */
    const stillCurrent = () =>
      !this.disabled && this.ctx === ctx && this.activeTheme === theme;
    try {
      const isLocal = url.startsWith("/");
      let arrayBuffer: ArrayBuffer;

      if (isLocal) {
        const response = await fetch(url);
        arrayBuffer = await response.arrayBuffer();
      } else {
        // External URL — use cache
        const filename = url.split("/").pop() ?? url;
        arrayBuffer = await fetchWithCache(
          url,
          this.activeTheme.manifest.id,
          "sfx",
          filename,
        );
      }

      if (!stillCurrent()) return;
      const audioBuffer = await ctx.decodeAudioData(arrayBuffer);
      // The decode is itself a wait, so recheck before publishing the buffer.
      if (!stillCurrent()) return;
      // Key by every eventType that maps to this URL
      for (const et of eventTypes) {
        this.sfxBuffers.set(et, audioBuffer);
      }
      console.debug(`[SFX] Loaded ${url} → [${eventTypes.join(", ")}]`);
    } catch (err) {
      console.warn(`[SFX] Failed to load: ${url}`, err);
    }
  }

  private playTrack(): void {
    if (this.disabled || !this.ctx || !this.musicGain) return;

    const track = this.trackOrder[this.trackIndex];
    if (!track) return;

    const audio = new Audio(track.url);
    audio.crossOrigin = "anonymous";
    const source = this.ctx.createMediaElementSource(audio);
    source.connect(this.musicGain);

    this.currentAudio = audio;

    // Capture generation so the continuations below become no-ops if a context
    // change or stopMusic has occurred since this track started.
    const gen = this.generation;
    audio.addEventListener("ended", () => {
      if (this.disabled || this.generation !== gen) return;
      this.crossfadeTo(this.nextTrackIndex());
    });

    // The guard at the top of this method is synchronous and stale by the time
    // a rejection arrives: `disabled` can latch, and the generation can move,
    // between initiating playback and hearing back. Neither `resume()` nor the
    // retry reaches a guarded entry point, so each async step rechecks both.
    audio.play().catch((err) => {
      console.warn("[music] play() rejected:", err);
      if (this.disabled || this.generation !== gen) return;
      if (this.ctx?.state === "suspended") {
        this.ctx.resume().then(() => {
          if (this.disabled || this.generation !== gen) return;
          audio.play().catch(() => {});
        });
      }
    });
  }

  private crossfadeTo(nextIndex: number, duration = 2.5): void {
    if (!this.ctx || !this.musicGain) return;

    this.crossfadeInProgress = true;

    const now = this.ctx.currentTime;
    const prefs = usePreferencesStore.getState();
    const targetVolume = this.computeEffectiveMusicGain(prefs);

    // Fade out current
    this.musicGain.gain.cancelScheduledValues(now);
    this.musicGain.gain.setValueAtTime(this.musicGain.gain.value, now);
    this.musicGain.gain.linearRampToValueAtTime(0, now + duration);

    const oldAudio = this.currentAudio;
    const gen = this.generation;

    setTimeout(() => {
      oldAudio?.pause();
      this.crossfadeInProgress = false;

      // Bail if a context change occurred during the crossfade
      if (this.generation !== gen) return;

      this.trackIndex = nextIndex;
      this.playTrack();

      // Fade in new
      if (this.musicGain && this.ctx) {
        const fadeInNow = this.ctx.currentTime;
        this.musicGain.gain.cancelScheduledValues(fadeInNow);
        this.musicGain.gain.setValueAtTime(0, fadeInNow);
        this.musicGain.gain.linearRampToValueAtTime(
          targetVolume,
          fadeInNow + duration,
        );
      }
    }, duration * 1000);
  }

  private nextTrackIndex(): number {
    const next = this.trackIndex + 1;
    if (next >= this.trackOrder.length) {
      // Re-shuffle from current context tracks
      const tracks = this.activeTheme.musicByContext[this.activeContext];
      if (this.activeContext === "battlefield") {
        const phaseFiltered = tracks.filter(
          (t) => t.phase === "any" || t.phase === this.battlefieldPhase,
        );
        this.trackOrder = shuffle(
          phaseFiltered.length > 0 ? [...phaseFiltered] : [...tracks],
        );
      } else {
        this.trackOrder = shuffle([...tracks]);
      }
      return 0;
    }
    return next;
  }

  private computeEffectiveSfxGain(
    prefs: ReturnType<typeof usePreferencesStore.getState>,
  ): number {
    if (prefs.masterMuted || prefs.sfxMuted) return 0;
    return (prefs.masterVolume / 100) * (prefs.sfxVolume / 100);
  }

  private computeEffectiveMusicGain(
    prefs: ReturnType<typeof usePreferencesStore.getState>,
  ): number {
    if (prefs.masterMuted || prefs.musicMuted) return 0;
    return (prefs.masterVolume / 100) * (prefs.musicVolume / 100);
  }

  /** Cancel any in-flight gain automation and restore music gain to target volume. */
  private resetMusicGain(): void {
    if (!this.musicGain || !this.ctx) return;
    const now = this.ctx.currentTime;
    this.musicGain.gain.cancelScheduledValues(now);
    const prefs = usePreferencesStore.getState();
    this.musicGain.gain.setValueAtTime(
      this.computeEffectiveMusicGain(prefs),
      now,
    );
  }

  private applySavedGains(): void {
    if (!this.sfxGain || !this.musicGain) return;
    const prefs = usePreferencesStore.getState();
    this.sfxGain.gain.value = this.computeEffectiveSfxGain(prefs);
    this.musicGain.gain.value = this.computeEffectiveMusicGain(prefs);
  }
}

export const audioManager = new AudioManager();

/**
 * Attach one-shot interaction listeners to warm up AudioContext (iOS/iPadOS)
 * and load the user's selected audio theme.
 */
export function initAudioOnInteraction(): void {
  const handler = async () => {
    // Remove listeners immediately to prevent double-fire (safe in StrictMode)
    document.removeEventListener("click", handler);
    document.removeEventListener("touchstart", handler);
    document.removeEventListener("keydown", handler);

    audioManager.warmUp();
    try {
      const prefs = usePreferencesStore.getState();
      const manifest = await findManifest(
        prefs.audioThemeId,
        prefs.customThemeUrls,
      );
      await audioManager.loadTheme(manifest);
    } catch (err) {
      console.warn("Failed to load audio theme, falling back to Planeswalker:", err);
      await audioManager.loadTheme(PLANESWALKER_THEME);
    }
    // useAudioContext may have already fired before warmUp completed,
    // so re-apply the current context to start music playback.
    audioManager.ensurePlayback();
  };
  document.addEventListener("click", handler);
  document.addEventListener("touchstart", handler);
  document.addEventListener("keydown", handler);
}

// Subscribe to preferences changes for real-time volume updates
usePreferencesStore.subscribe(() => audioManager.updateVolumes());
