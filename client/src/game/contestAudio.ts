import { usePreferencesStore } from "../stores/preferencesStore.ts";

export type ContestCue = "tick" | "go" | "win" | "lose" | "neutral";

/** [frequency Hz, start offset s, duration s] per note. */
const CUES: Record<ContestCue, ReadonlyArray<readonly [number, number, number]>> = {
  tick: [[440, 0, 0.08]],
  go: [[880, 0, 0.25]],
  win: [[523, 0, 0.12], [659, 0.12, 0.12], [784, 0.24, 0.25]],
  lose: [[392, 0, 0.15], [330, 0.15, 0.15], [262, 0.3, 0.3]],
  neutral: [[440, 0, 0.12], [440, 0.18, 0.12]],
};

let context: AudioContext | null = null;

/**
 * Short synthesized cue for the contest countdown and result. Audio is a
 * redundant channel: every state change is also shown as text, so a muted,
 * blocked, or unsupported audio stack silently does nothing.
 */
export function playContestCue(cue: ContestCue): void {
  try {
    const { sfxMuted, sfxVolume } = usePreferencesStore.getState();
    if (sfxMuted || sfxVolume <= 0 || typeof AudioContext === "undefined") return;
    context ??= new AudioContext();
    const ctx = context;
    if (ctx.state === "suspended") void ctx.resume();
    const peak = 0.15 * (sfxVolume / 100);
    for (const [frequency, offset, duration] of CUES[cue]) {
      const oscillator = ctx.createOscillator();
      const gain = ctx.createGain();
      const start = ctx.currentTime + offset;
      oscillator.frequency.value = frequency;
      gain.gain.setValueAtTime(peak, start);
      gain.gain.exponentialRampToValueAtTime(0.0001, start + duration);
      oscillator.connect(gain).connect(ctx.destination);
      oscillator.start(start);
      oscillator.stop(start + duration);
    }
  } catch {
    // Audio is best-effort.
  }
}
