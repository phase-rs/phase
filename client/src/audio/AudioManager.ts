import type { StepEffect } from "../animation/types";
import { usePreferencesStore } from "../stores/preferencesStore";

import { MUSIC_TRACKS, SFX_MAP } from "./sfxMap";

/** Fisher-Yates shuffle (in-place). */
function shuffle<T>(arr: T[]): T[] {
  for (let i = arr.length - 1; i > 0; i--) {
    const j = Math.floor(Math.random() * (i + 1));
    [arr[i], arr[j]] = [arr[j], arr[i]];
  }
  return arr;
}

class AudioManager {
  private ctx: AudioContext | null = null;
  private sfxBuffers = new Map<string, AudioBuffer>();
  private sfxGain: GainNode | null = null;
  private musicGain: GainNode | null = null;
  private currentAudio: HTMLAudioElement | null = null;
  private trackOrder: string[] = [];
  private trackIndex = 0;
  private isWarmedUp = false;

  /** Create AudioContext and gain nodes. Apply saved volume preferences. */
  warmUp(): void {
    if (this.isWarmedUp) return;
    this.ctx = new AudioContext();
    this.sfxGain = this.ctx.createGain();
    this.sfxGain.connect(this.ctx.destination);
    this.musicGain = this.ctx.createGain();
    this.musicGain.connect(this.ctx.destination);

    const prefs = usePreferencesStore.getState();
    if (prefs.masterMuted) {
      this.sfxGain.gain.value = 0;
      this.musicGain.gain.value = 0;
    } else {
      this.sfxGain.gain.value = prefs.sfxMuted ? 0 : prefs.sfxVolume / 100;
      this.musicGain.gain.value = prefs.musicMuted
        ? 0
        : prefs.musicVolume / 100;
    }
    this.isWarmedUp = true;
  }

  /** Preload all unique SFX files into AudioBuffers. */
  async preloadSfx(): Promise<void> {
    if (!this.ctx) return;
    const unique = [...new Set(Object.values(SFX_MAP))];
    await Promise.all(unique.map((filename) => this.loadBuffer(filename)));
  }

  /** Play a single SFX by GameEvent type. */
  playSfx(eventType: string, volume = 1.0): void {
    if (!this.ctx || !this.sfxGain) return;

    const filename = SFX_MAP[eventType];
    if (!filename) return;

    const buffer = this.sfxBuffers.get(filename);
    if (!buffer) return;

    const prefs = usePreferencesStore.getState();
    if (prefs.sfxMuted || prefs.masterMuted) return;

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
      typeCounts.set(effect.type, (typeCounts.get(effect.type) ?? 0) + 1);
    }

    for (const [type, count] of typeCounts) {
      if (!SFX_MAP[type]) continue;
      const volume =
        count > 1 ? Math.min(1.0 + count * 0.15, 1.5) : 1.0;
      this.playSfx(type, volume);
    }
  }

  /** Start music playback with shuffled track rotation. */
  startMusic(): void {
    if (!this.ctx || !this.musicGain) return;

    const prefs = usePreferencesStore.getState();
    if (prefs.musicMuted && prefs.masterMuted) return;

    this.trackOrder = shuffle([...MUSIC_TRACKS]);
    this.trackIndex = 0;
    this.playTrack();
  }

  /** Stop music with optional fade-out. */
  stopMusic(fadeOut = 2.0): void {
    if (!this.ctx || !this.musicGain || !this.currentAudio) return;

    const now = this.ctx.currentTime;
    this.musicGain.gain.cancelScheduledValues(now);
    this.musicGain.gain.setValueAtTime(this.musicGain.gain.value, now);
    this.musicGain.gain.linearRampToValueAtTime(0, now + fadeOut);

    const audio = this.currentAudio;
    setTimeout(() => {
      audio.pause();
    }, fadeOut * 1000);

    this.currentAudio = null;
  }

  /**
   * Resume audio playback after a user gesture (e.g. unmute button click).
   * Warms up the AudioContext if needed, resumes it if suspended,
   * and starts music if no track is currently playing.
   */
  ensurePlayback(): void {
    this.warmUp();
    this.preloadSfx();

    if (this.ctx?.state === "suspended") {
      this.ctx.resume();
    }

    if (!this.currentAudio) {
      this.startMusic();
    }
  }

  /** Read current preferences and update gain node values. */
  updateVolumes(): void {
    if (!this.sfxGain || !this.musicGain || !this.ctx) return;

    const prefs = usePreferencesStore.getState();
    const now = this.ctx.currentTime;

    this.sfxGain.gain.cancelScheduledValues(now);
    this.sfxGain.gain.setValueAtTime(this.sfxGain.gain.value, now);

    this.musicGain.gain.cancelScheduledValues(now);
    this.musicGain.gain.setValueAtTime(this.musicGain.gain.value, now);

    if (prefs.masterMuted) {
      this.sfxGain.gain.value = 0;
      this.musicGain.gain.value = 0;
    } else {
      this.sfxGain.gain.value = prefs.sfxMuted ? 0 : prefs.sfxVolume / 100;
      this.musicGain.gain.value = prefs.musicMuted
        ? 0
        : prefs.musicVolume / 100;
    }
  }

  /** Stop music, close AudioContext. */
  dispose(): void {
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

  private async loadBuffer(filename: string): Promise<void> {
    if (!this.ctx) return;
    try {
      const response = await fetch(`/audio/sfx/${filename}.mp3`);
      const arrayBuffer = await response.arrayBuffer();
      const audioBuffer = await this.ctx.decodeAudioData(arrayBuffer);
      this.sfxBuffers.set(filename, audioBuffer);
    } catch (err) {
      console.warn(`Failed to load SFX: ${filename}`, err);
    }
  }

  private playTrack(): void {
    if (!this.ctx || !this.musicGain) return;

    const trackName = this.trackOrder[this.trackIndex];
    const audio = new Audio(`/audio/music/${trackName}.mp3`);
    const source = this.ctx.createMediaElementSource(audio);
    source.connect(this.musicGain);

    this.currentAudio = audio;

    audio.addEventListener("ended", () => {
      this.crossfadeTo(this.nextTrackIndex());
    });

    audio.play();
  }

  private crossfadeTo(nextIndex: number, duration = 2.5): void {
    if (!this.ctx || !this.musicGain) return;

    const now = this.ctx.currentTime;
    const prefs = usePreferencesStore.getState();
    const targetVolume = prefs.musicMuted ? 0 : prefs.musicVolume / 100;

    // Fade out current
    this.musicGain.gain.cancelScheduledValues(now);
    this.musicGain.gain.setValueAtTime(this.musicGain.gain.value, now);
    this.musicGain.gain.linearRampToValueAtTime(0, now + duration);

    const oldAudio = this.currentAudio;

    setTimeout(() => {
      oldAudio?.pause();
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
      // Re-shuffle and restart
      this.trackOrder = shuffle([...MUSIC_TRACKS]);
      return 0;
    }
    return next;
  }
}

export const audioManager = new AudioManager();

/** Attach one-shot interaction listeners to warm up AudioContext (iOS/iPadOS). */
export function initAudioOnInteraction(): void {
  const handler = () => {
    audioManager.warmUp();
    audioManager.preloadSfx();
    document.removeEventListener("click", handler);
    document.removeEventListener("touchstart", handler);
    document.removeEventListener("keydown", handler);
  };
  document.addEventListener("click", handler);
  document.addEventListener("touchstart", handler);
  document.addEventListener("keydown", handler);
}

// Subscribe to preferences changes for real-time volume updates
usePreferencesStore.subscribe(() => audioManager.updateVolumes());
