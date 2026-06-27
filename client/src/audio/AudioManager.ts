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

class AudioManager {
  private ctx: AudioContext | null = null;
  private sfxBuffers = new Map<string, AudioBuffer>();
  private sfxGain: GainNode | null = null;
  private musicGain: GainNode | null = null;
  private currentAudio: HTMLAudioElement | null = null;
  private trackOrder: ThemeTrack[] = [];
  private trackIndex = 0;
  private isWarmedUp = false;
  private crossfadeInProgress = false;
  /** Incremented on every context/theme change to invalidate stale timeouts. */
  private generation = 0;
  /** Separate element for victory/defeat stingers, so setContext can stop them. */
  private stingerAudio: HTMLAudioElement | null = null;

  // Theme & context state
  private activeTheme: ResolvedTheme = resolveTheme(PLANESWALKER_THEME);
  private activeContext: AudioContextName = "menu";
  private battlefieldPhase: GamePhaseTag = "early";

  /** Create AudioContext and gain nodes. Apply saved volume preferences. */
  warmUp(): void {
    if (this.isWarmedUp) return;
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

  /** Preload all unique SFX files into AudioBuffers (background, non-blocking). */
  async preloadSfx(): Promise<void> {
    if (!this.ctx) return;
    const urls = [...new Set(Object.values(this.activeTheme.sfxMap))];
    const entries = Object.entries(this.activeTheme.sfxMap);

    await Promise.all(
      urls.map(async (url) => {
        // Find the eventType(s) that map to this URL
        const eventTypes = entries
          .filter(([_, u]) => u === url)
          .map(([et]) => et);
        await this.loadBuffer(url, eventTypes);
      }),
    );
  }

  /** Play a single SFX by GameEvent type. */
  playSfx(eventType: string, volume = 1.0): void {
    if (!this.ctx || !this.sfxGain) return;

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

    if (!this.ctx || !this.musicGain) return;

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
    if (!this.ctx || !this.musicGain) return;

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
   */
  ensurePlayback(): void {
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
    const ctxState = this.ctx?.state ?? "none";
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

  private async loadBuffer(url: string, eventTypes: string[]): Promise<void> {
    if (!this.ctx) return;
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

      const audioBuffer = await this.ctx.decodeAudioData(arrayBuffer);
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
    if (!this.ctx || !this.musicGain) return;

    const track = this.trackOrder[this.trackIndex];
    if (!track) return;

    const audio = new Audio(track.url);
    audio.crossOrigin = "anonymous";
    const source = this.ctx.createMediaElementSource(audio);
    source.connect(this.musicGain);

    this.currentAudio = audio;

    // Capture generation so the ended handler becomes a no-op if a context
    // change or stopMusic has occurred since this track started.
    const gen = this.generation;
    audio.addEventListener("ended", () => {
      if (this.generation !== gen) return;
      this.crossfadeTo(this.nextTrackIndex());
    });

    audio.play().catch((err) => {
      console.warn("[music] play() rejected:", err);
      if (this.ctx?.state === "suspended") {
        this.ctx.resume().then(() => audio.play().catch(() => {}));
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
