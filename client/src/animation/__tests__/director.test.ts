import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { audioManager } from "../../audio/AudioManager";
import { useAnimationStore } from "../../stores/animationStore";
import { usePreferencesStore } from "../../stores/preferencesStore";
import { buildGameState, buildStackEntry } from "../../test/factories/gameStateFactory";
import { resetStackThroughput } from "../../utils/stackThroughput";
import { abandonPendingDispatches } from "../../game/dispatch";
import { director, PRESENT_GRACE_MS } from "../director";
import type { AnimationStep } from "../types";

/** One presentation step as `normalizeEvents` produces it: a life change, 1 SFX. */
function step(duration: number): AnimationStep {
  return {
    duration,
    effects: [
      { event: { type: "LifeChanged", data: { player_id: 1, amount: -1 } }, duration },
    ],
  };
}

function stateWithStack(size = 0) {
  return buildGameState({
    stack: Array.from({ length: size }, (_, index) => buildStackEntry({ id: index + 1 })),
  });
}

function setDocumentHidden(hidden: boolean): void {
  Object.defineProperty(document, "hidden", { configurable: true, get: () => hidden });
}

/** Track settlement without awaiting, so "not yet" is observable. */
function track(settled: Promise<void>): { done: () => boolean } {
  let done = false;
  void settled.then(() => {
    done = true;
  });
  return { done: () => done };
}

describe("director", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    resetStackThroughput();
    usePreferencesStore.setState({ animationSpeedMultiplier: 1 });
    useAnimationStore.getState().clearQueue();
  });

  afterEach(() => {
    director.abandon();
    useAnimationStore.getState().clearQueue();
    setDocumentHidden(false);
    vi.restoreAllMocks();
    vi.useRealTimers();
  });

  it.each([1, 0.5, 2])("play resolves after the total beat time at pace %s", async (pace) => {
    usePreferencesStore.setState({ animationSpeedMultiplier: pace });
    const run = director.play({ steps: [step(200), step(300)], snapshotSeq: 1, state: stateWithStack() });
    const settlement = track(run);

    await vi.advanceTimersByTimeAsync(500 * pace - 1);
    expect(settlement.done()).toBe(false);

    await vi.advanceTimersByTimeAsync(1);
    await run;
    expect(settlement.done()).toBe(true);
    expect(useAnimationStore.getState().activeStep).toBeNull();
  });

  it("applies pace once: speed × stack pressure", async () => {
    usePreferencesStore.setState({ animationSpeedMultiplier: 0.5 });
    // 12 entries is Elevated stack pressure (×0.5), so the applied pace is 0.25.
    const run = director.play({ steps: [step(200), step(300)], snapshotSeq: 1, state: stateWithStack(12) });
    expect(useAnimationStore.getState().pace).toBe(0.25);
    const settlement = track(run);

    // 500 ms × 0.25 = 125 ms. A double application would have finished at 31.25 ms.
    await vi.advanceTimersByTimeAsync(124);
    expect(settlement.done()).toBe(false);

    await vi.advanceTimersByTimeAsync(1);
    await run;
    expect(settlement.done()).toBe(true);
  });

  it("skip('all') settles at once and plays no further SFX", async () => {
    const sfx = vi.spyOn(audioManager, "playSfxForStep");
    const run = director.play({ steps: [step(300), step(300), step(300)], snapshotSeq: 1, state: stateWithStack() });
    const played = sfx.mock.calls.length;
    expect(played).toBe(1);

    director.skip("all");
    await run;
    expect(useAnimationStore.getState().activeStep).toBeNull();
    expect(useAnimationStore.getState().queue).toEqual([]);

    await vi.runAllTimersAsync();
    expect(sfx.mock.calls.length).toBe(played);
  });

  it("skip('block') advances exactly one beat", async () => {
    const run = director.play({ steps: [step(300), step(300)], snapshotSeq: 1, state: stateWithStack() });
    const first = useAnimationStore.getState().activeStep;

    director.skip("block");
    const second = useAnimationStore.getState().activeStep;
    expect(second).not.toBeNull();
    expect(second).not.toBe(first);

    director.skip("block");
    await run;
    expect(useAnimationStore.getState().activeStep).toBeNull();
  });

  it("speed 0 fires every SFX at once and queues nothing", async () => {
    usePreferencesStore.setState({ animationSpeedMultiplier: 0 });
    const sfx = vi.spyOn(audioManager, "playSfxForStep");
    const steps = [step(300), step(200)];

    await director.play({ steps, snapshotSeq: 1, state: stateWithStack() });

    expect(sfx).toHaveBeenCalledTimes(2);
    expect(sfx.mock.calls[0][0]).toEqual(steps[0].effects);
    expect(useAnimationStore.getState().activeStep).toBeNull();
    expect(useAnimationStore.getState().queue).toEqual([]);
    expect(vi.getTimerCount()).toBe(0);
  });

  it("queues nothing when the tab is hidden at entry", async () => {
    setDocumentHidden(true);
    const sfx = vi.spyOn(audioManager, "playSfxForStep");

    await director.play({ steps: [step(300)], snapshotSeq: 1, state: stateWithStack() });

    expect(sfx).not.toHaveBeenCalled();
    expect(useAnimationStore.getState().activeStep).toBeNull();
    expect(useAnimationStore.getState().queue).toEqual([]);
    expect(vi.getTimerCount()).toBe(0);
  });

  it("resolves play() when the tab hides mid-play and plays no later SFX", async () => {
    const sfx = vi.spyOn(audioManager, "playSfxForStep");
    const run = director.play({ steps: [step(300), step(300)], snapshotSeq: 1, state: stateWithStack() });
    await vi.advanceTimersByTimeAsync(10);
    const played = sfx.mock.calls.length;

    setDocumentHidden(true);
    document.dispatchEvent(new Event("visibilitychange"));
    await run;

    expect(useAnimationStore.getState().activeStep).toBeNull();
    expect(useAnimationStore.getState().queue).toEqual([]);
    await vi.runAllTimersAsync();
    expect(sfx.mock.calls.length).toBe(played);
  });

  it("abandon() resolves an in-flight play()", async () => {
    const run = director.play({ steps: [step(300), step(300)], snapshotSeq: 1, state: stateWithStack() });
    const settlement = track(run);
    await vi.advanceTimersByTimeAsync(10);
    expect(settlement.done()).toBe(false);

    director.abandon();
    await run;
    expect(settlement.done()).toBe(true);
    expect(useAnimationStore.getState().activeStep).toBeNull();
  });

  it("plays no step SFX for a slam-deferred hit", async () => {
    const sfx = vi.spyOn(audioManager, "playSfxForStep");
    const hit: AnimationStep = {
      duration: 300,
      effects: [{
        event: {
          type: "DamageDealt",
          data: { source_id: 1, target: { Player: 1 }, amount: 2, is_combat: true },
        },
        duration: 300,
      }],
    };
    const run = director.play({ steps: [hit], snapshotSeq: 1, state: stateWithStack() });
    await vi.advanceTimersByTimeAsync(300);
    await run;
    expect(sfx).not.toHaveBeenCalled();
  });

  it("fires a presenter cue due at the beat's end before advancing, however late it presents", async () => {
    const detach = director.attachPresenter();
    const run = director.play({ steps: [step(300)], snapshotSeq: 1, state: stateWithStack() });
    const active = useAnimationStore.getState().activeStep!;

    // The render lands 400 ms late, past the beat's own duration.
    await vi.advanceTimersByTimeAsync(400);
    expect(useAnimationStore.getState().activeStep).toBe(active);
    let activeAtCue: unknown;
    setTimeout(() => {
      activeAtCue = useAnimationStore.getState().activeStep;
    }, 300);
    director.presented(active);

    await vi.advanceTimersByTimeAsync(300);
    await run;
    expect(activeAtCue).toBe(active);
    detach();
  });

  it("still settles when an attached presenter never reports the step", async () => {
    const detach = director.attachPresenter();
    const run = director.play({ steps: [step(300)], snapshotSeq: 1, state: stateWithStack() });
    const settlement = track(run);

    await vi.advanceTimersByTimeAsync(300 + PRESENT_GRACE_MS - 1);
    expect(settlement.done()).toBe(false);
    await vi.advanceTimersByTimeAsync(1);
    await run;
    expect(settlement.done()).toBe(true);
    detach();
  });

  it("hiding an idle tab keeps a flight veil", () => {
    useAnimationStore.getState().veilFlight(7);
    setDocumentHidden(true);
    document.dispatchEvent(new Event("visibilitychange"));
    expect(useAnimationStore.getState().flightVeiledObjectIds.has(7)).toBe(true);
  });

  it("abandoning dispatches settles play(), unless a finished game keeps its presentation", async () => {
    const run = director.play({ steps: [step(300)], snapshotSeq: 1, state: stateWithStack() });
    const settlement = track(run);

    abandonPendingDispatches({ keepPresentation: true });
    await vi.advanceTimersByTimeAsync(0);
    expect(settlement.done()).toBe(false);
    expect(useAnimationStore.getState().activeStep).not.toBeNull();

    abandonPendingDispatches();
    await run;
    expect(useAnimationStore.getState().activeStep).toBeNull();
  });

  it("abandon() leaves a flight veil to the effect still drawing it", () => {
    useAnimationStore.getState().veilFlight(7);
    director.abandon();
    expect(useAnimationStore.getState().flightVeiledObjectIds.has(7)).toBe(true);
  });

  it("advances a steps array enqueued directly into the store, without SFX", async () => {
    const sfx = vi.spyOn(audioManager, "playSfxForStep");
    useAnimationStore.getState().enqueueSteps([step(300)], 1);
    expect(useAnimationStore.getState().activeStep).not.toBeNull();

    await vi.advanceTimersByTimeAsync(299);
    expect(useAnimationStore.getState().activeStep).not.toBeNull();

    await vi.advanceTimersByTimeAsync(1);
    expect(useAnimationStore.getState().activeStep).toBeNull();
    expect(sfx).not.toHaveBeenCalled();
  });

  it("records the newest presentation snapshot a kept abandon strands", async () => {
    const run = director.play({ steps: [step(200)], snapshotSeq: 7, state: stateWithStack() });
    // The step drains first; its flights may still be drawing when the game
    // ends, so the abandoned snapshot must be remembered past the queue.
    await vi.advanceTimersByTimeAsync(200 + PRESENT_GRACE_MS);
    await run;
    expect(useAnimationStore.getState().activeStep).toBeNull();

    abandonPendingDispatches({ keepPresentation: true });

    expect(useAnimationStore.getState().abandonedSnapshotSeq).toBeGreaterThanOrEqual(7);
  });

  it("marks the snapshot abandoned when the presentation is dropped too", () => {
    useAnimationStore.getState().enqueueSteps([step(200)], 8);
    abandonPendingDispatches();
    expect(useAnimationStore.getState().abandonedSnapshotSeq).toBeGreaterThanOrEqual(8);
  });

  it("hiding an active presentation drops its steps and keeps its flight veil", async () => {
    const run = director.play({ steps: [step(300)], snapshotSeq: 9, state: stateWithStack() });
    useAnimationStore.getState().veilFlight(7);

    setDocumentHidden(true);
    document.dispatchEvent(new Event("visibilitychange"));
    await run;

    expect(useAnimationStore.getState().activeStep).toBeNull();
    expect(useAnimationStore.getState().flightVeiledObjectIds.has(7)).toBe(true);
    // The action still commits, so its snapshot is not abandoned.
    expect(useAnimationStore.getState().abandonedSnapshotSeq).toBeLessThan(9);
  });
});
