import type { GameState } from "../adapter/types";
import { audioManager } from "../audio/AudioManager";
import { useAnimationStore, type AnimationStore } from "../stores/animationStore";
import { usePreferencesStore } from "../stores/preferencesStore";
import { pressureMultiplier } from "../utils/stackPressure";
import { effectiveStackPressure } from "../utils/stackThroughput";
import { compileBeat } from "./compileStepsToBeats";
import type { AnimationStep } from "./types";

/** One action's presentation, as the dispatcher hands it to the Director. */
export interface DirectorInput {
  /** Normalized steps for this action, in play order. */
  steps: readonly AnimationStep[];
  /** The engine snapshot this action animates; stamped on every queued step. */
  snapshotSeq: number;
  /** Post-action state: supplies `stack.length` for pressure and the overlay's `animationNewState`. */
  state: GameState;
}

export interface Director {
  /** Queue one action's presentation and resolve once its last step has played. */
  play(input: DirectorInput): Promise<void>;
  /** End the current beat now, or settle the whole queue at once. Skipped beats play no SFX. */
  skip(scope: "block" | "all"): void;
  /**
   * Apply the one pace presentation runs at: speed preference × stack pressure.
   * The clock keeps the active beat's timer and uses this from the next arm.
   * Calling it mid-step also makes a mounted overlay re-present that step, so
   * `play()` calls it only before its first beat.
   */
  setPace(pace: number): void;
  /**
   * Register a mounted presenter (the overlay). While one is attached, a beat's
   * clock starts when the presenter reports it presented, not when the store
   * promotes it. Returns the detach function.
   */
  attachPresenter(): () => void;
  /** The presenter has scheduled `step`'s own cues: start the beat's clock now. */
  presented(step: AnimationStep): void;
  /** Session boundary: settle every in-flight `play()` and clear the presentation. */
  abandon(): void;
}

/** The one pace presentation runs at, applied once: speed preference × stack pressure. */
export function directorPace(
  speedMultiplier: number,
  stackLength: number,
  now: number = performance.now(),
): number {
  return Math.max(0, speedMultiplier * pressureMultiplier(effectiveStackPressure(stackLength, now)));
}

/**
 * The Director owns the only clock. The animation store holds the queue and
 * the active step; this module subscribes to it once and arms the single
 * `setTimeout` for the active beat's duration at the applied pace. The overlay
 * schedules nothing.
 *
 * Consequence: `play()` resolves only when its own steps (tagged with a
 * `runId`) have left the store, so a dispatch commit can never outrun the
 * presentation under stack pressure. Direct `enqueueSteps` callers (tests, the
 * VFX gallery) still advance, but without SFX: only a `play()` run plays SFX.
 */
/**
 * How long past its own duration a beat may wait for an attached presenter to
 * report it. Only a stalled or failed render gets this far; the watchdog keeps
 * `play()` (and the dispatch mutex behind it) from waiting forever.
 */
export const PRESENT_GRACE_MS = 1000;

let presenters = 0;
let runSeq = 0;
let scheduledGeneration = -1;
let stepTimer: ReturnType<typeof setTimeout> | null = null;

function isDocumentHidden(): boolean {
  return typeof document !== "undefined" && document.hidden;
}

function clearStepTimer(): void {
  if (stepTimer !== null) {
    clearTimeout(stepTimer);
    stepTimer = null;
  }
}

/** The Director's applied pace, or the speed preference before any `play()`. */
function effectivePace(state: AnimationStore): number {
  return state.pace ?? usePreferencesStore.getState().animationSpeedMultiplier;
}

function hasRun(state: AnimationStore, runId: number): boolean {
  return state.activeStep?.runId === runId || state.queue.some((step) => step.runId === runId);
}

/**
 * One store notification. Every step change (promotion, advance, clear) bumps
 * `activeGeneration`, so this is the only signal the clock needs; other store
 * writes are ignored.
 */
function handleStepChange(): void {
  const state = useAnimationStore.getState();
  if (state.activeGeneration === scheduledGeneration) return;
  scheduledGeneration = state.activeGeneration;
  clearStepTimer();

  const step = state.activeStep;
  if (!step) return;

  // Armed first, so a cue that throws cannot leave the step stuck.
  const grace = presenters > 0 ? PRESENT_GRACE_MS : 0;
  armStepTimer(step.duration * effectivePace(state) + grace);
  const beat = compileBeat(step);
  // Only a play() run is audible; a direct enqueue is a test or a preview.
  if (step.runId !== undefined && beat.sfx.length > 0) {
    audioManager.playSfxForStep(beat.sfx);
  }
}

function armStepTimer(delayMs: number): void {
  clearStepTimer();
  stepTimer = setTimeout(() => {
    stepTimer = null;
    useAnimationStore.getState().advanceStep();
  }, Math.max(0, delayMs));
}

function attachPresenter(): () => void {
  presenters += 1;
  return () => {
    presenters -= 1;
  };
}

/**
 * The presenter's cue timers exist by now, so a cue due exactly at the beat's
 * end fires before the step advances, and a slow render cannot eat into the
 * beat.
 */
function presented(step: AnimationStep): void {
  const state = useAnimationStore.getState();
  if (state.activeStep !== step) return;
  armStepTimer(step.duration * effectivePace(state));
}

/** A hidden tab renders no frames and nobody hears it: settle the presentation at once. */
function handleVisibilityChange(): void {
  if (!isDocumentHidden()) return;
  // Only a presentation in progress is dropped: an idle store may still hold a
  // flight veil, or a landed life total waiting on its commit.
  if (useAnimationStore.getState().activeStep) useAnimationStore.getState().clearQueue();
}

const unsubscribeClock = useAnimationStore.subscribe(handleStepChange);
if (typeof document !== "undefined") {
  document.addEventListener("visibilitychange", handleVisibilityChange);
}
// A hot update re-evaluates this module; without this the old subscription
// would keep running as a second clock and advance every step twice.
import.meta.hot?.dispose(() => {
  unsubscribeClock();
  clearStepTimer();
  if (typeof document !== "undefined") {
    document.removeEventListener("visibilitychange", handleVisibilityChange);
  }
});
// Adopt a step that was already active before this module loaded (a dynamic
// import or an HMR re-evaluation).
handleStepChange();

function waitForRun(runId: number): Promise<void> {
  if (!hasRun(useAnimationStore.getState(), runId)) return Promise.resolve();
  return new Promise<void>((resolve) => {
    const unsubscribe = useAnimationStore.subscribe((state) => {
      if (hasRun(state, runId)) return;
      unsubscribe();
      resolve();
    });
  });
}

function setPace(pace: number): void {
  useAnimationStore.getState().setPace(Math.max(0, pace));
}

async function play(input: DirectorInput): Promise<void> {
  const { steps, snapshotSeq, state } = input;
  if (steps.length === 0) return;
  // A hidden tab queues nothing: no steps, no SFX, no timers.
  if (isDocumentHidden()) return;

  const pace = directorPace(
    usePreferencesStore.getState().animationSpeedMultiplier,
    state.stack.length,
  );

  if (pace <= 0) {
    // Instant speed: no queue, every SFX at once (the legacy behavior). Pace is
    // not published: nothing is presented, so there is no run to pace.
    for (const step of steps) audioManager.playSfxForStep(step.effects);
    return;
  }

  // Publish the pace before the first beat is promoted, so its timer arms at it.
  setPace(pace);
  useAnimationStore.getState().setAnimationNewState(state);
  const runId = ++runSeq;
  useAnimationStore.getState().enqueueSteps(steps, snapshotSeq, runId);
  await waitForRun(runId);
}

// Skip and abandon drop the steps while the overlay and the VFX layer may still
// be drawing flights; un-veiling here would show a card beside its own flight.
function skip(scope: "block" | "all"): void {
  if (scope === "all") useAnimationStore.getState().clearQueue({ keepFlightVeils: true });
  else useAnimationStore.getState().advanceStep();
}

function abandon(): void {
  useAnimationStore.getState().clearQueue({ keepFlightVeils: true });
}

export const director: Director = { play, skip, setPace, attachPresenter, presented, abandon };
