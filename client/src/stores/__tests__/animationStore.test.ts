import { describe, it, expect, beforeEach, vi } from "vitest";
import { useAnimationStore } from "../animationStore";
import type { AnimationStep } from "../../animation/types";
import type { GameEvent } from "../../adapter/types";

describe("animationStore", () => {
  beforeEach(() => {
    useAnimationStore.getState().clearQueue();
  });

  const makeStep = (duration = 300): AnimationStep => ({
    effects: [{ event: { type: "DamageDealt", data: { amount: 3 } } as GameEvent, duration }],
    duration,
  });

  describe("enqueueSteps", () => {
    it("promotes first step to activeStep when idle", () => {
      const steps = [makeStep(300), makeStep(400)];
      useAnimationStore.getState().enqueueSteps(steps, 1);

      const state = useAnimationStore.getState();
      expect(state.activeStep).toEqual(expect.objectContaining(steps[0]));
      expect(state.queue).toHaveLength(1);
      expect(state.isPlaying).toBe(true);
    });

    it("stamps each step with the snapshot it animates", () => {
      useAnimationStore.getState().enqueueSteps([makeStep(), makeStep()], 3);
      useAnimationStore.getState().enqueueSteps([makeStep()], 4);

      const seqs = [useAnimationStore.getState().activeStep?.snapshotSeq];
      useAnimationStore.getState().advanceStep();
      seqs.push(useAnimationStore.getState().activeStep?.snapshotSeq);
      useAnimationStore.getState().advanceStep();
      seqs.push(useAnimationStore.getState().activeStep?.snapshotSeq);
      expect(seqs).toEqual([3, 3, 4]);
    });

    it("appends to queue when already playing", () => {
      useAnimationStore.getState().enqueueSteps([makeStep()], 1);
      useAnimationStore.getState().enqueueSteps([makeStep(), makeStep()], 1);

      const state = useAnimationStore.getState();
      expect(state.activeStep).toBeTruthy();
      expect(state.queue).toHaveLength(2);
    });
  });

  describe("veilObjects", () => {
    it("hides objects only for the step that veiled them", () => {
      useAnimationStore.getState().enqueueSteps([makeStep(), makeStep()], 1);
      useAnimationStore.getState().veilObjects([10, 11]);
      expect([...useAnimationStore.getState().veiledObjectIds]).toEqual([10, 11]);

      useAnimationStore.getState().advanceStep();
      expect(useAnimationStore.getState().veiledObjectIds.size).toBe(0);
    });

    it("never outlives the queue", () => {
      useAnimationStore.getState().enqueueSteps([makeStep()], 1);
      useAnimationStore.getState().veilObjects([10]);
      useAnimationStore.getState().advanceStep();
      expect(useAnimationStore.getState().veiledObjectIds.size).toBe(0);

      useAnimationStore.getState().enqueueSteps([makeStep()], 1);
      useAnimationStore.getState().veilObjects([12]);
      useAnimationStore.getState().clearQueue();
      expect(useAnimationStore.getState().veiledObjectIds.size).toBe(0);
    });
  });

  describe("flight veil", () => {
    it("survives every step advance, including the one that empties the queue", () => {
      useAnimationStore.getState().enqueueSteps([makeStep(), makeStep()], 1);
      useAnimationStore.getState().veilFlight(7);

      useAnimationStore.getState().advanceStep();
      expect(useAnimationStore.getState().flightVeiledObjectIds.has(7)).toBe(true);

      useAnimationStore.getState().advanceStep();
      expect(useAnimationStore.getState().activeStep).toBeNull();
      expect(useAnimationStore.getState().flightVeiledObjectIds.has(7)).toBe(true);
    });

    it("keeps the flight veil while a step advance clears the step veil", () => {
      useAnimationStore.getState().enqueueSteps([makeStep(), makeStep()], 1);
      useAnimationStore.getState().veilObjects([8]);
      useAnimationStore.getState().veilFlight(7);

      useAnimationStore.getState().advanceStep();

      const state = useAnimationStore.getState();
      expect(state.veiledObjectIds.size).toBe(0);
      expect(state.flightVeiledObjectIds.has(7)).toBe(true);
    });

    it("releases only the unveiled object", () => {
      useAnimationStore.getState().veilFlight(7);
      useAnimationStore.getState().veilFlight(9);
      useAnimationStore.getState().unveilFlight(7);

      expect([...useAnimationStore.getState().flightVeiledObjectIds]).toEqual([9]);
    });

    it("is cleared by clearQueue", () => {
      useAnimationStore.getState().veilFlight(7);
      useAnimationStore.getState().clearQueue();

      expect(useAnimationStore.getState().flightVeiledObjectIds.size).toBe(0);
    });

    it("does not touch the step veil", () => {
      useAnimationStore.getState().veilFlight(7);

      expect(useAnimationStore.getState().veiledObjectIds.size).toBe(0);
    });
  });

  describe("advanceStep", () => {
    it("advances through steps in order", () => {
      const step1 = makeStep(100);
      const step2 = makeStep(200);
      useAnimationStore.getState().enqueueSteps([step1, step2], 1);

      expect(useAnimationStore.getState().activeStep).toEqual(expect.objectContaining(step1));
      expect(useAnimationStore.getState().isPlaying).toBe(true);

      useAnimationStore.getState().advanceStep();
      expect(useAnimationStore.getState().activeStep).toEqual(expect.objectContaining(step2));
      expect(useAnimationStore.getState().isPlaying).toBe(true);

      useAnimationStore.getState().advanceStep();
      expect(useAnimationStore.getState().activeStep).toBeNull();
      expect(useAnimationStore.getState().isPlaying).toBe(false);
    });

    it("clears when queue is empty", () => {
      useAnimationStore.getState().enqueueSteps([makeStep()], 1);
      useAnimationStore.getState().advanceStep();

      const state = useAnimationStore.getState();
      expect(state.activeStep).toBeNull();
      expect(state.isPlaying).toBe(false);
    });
  });

  describe("clearQueue", () => {
    it("resets all animation state", () => {
      useAnimationStore.getState().enqueueSteps([makeStep(), makeStep()], 1);
      expect(useAnimationStore.getState().isPlaying).toBe(true);

      useAnimationStore.getState().clearQueue();

      const state = useAnimationStore.getState();
      expect(state.queue).toHaveLength(0);
      expect(state.activeStep).toBeNull();
      expect(state.isPlaying).toBe(false);
    });
  });

  describe("captureSnapshot", () => {
    it("reads data-object-id elements from DOM", () => {
      const mockRect = {
        x: 10,
        y: 20,
        width: 100,
        height: 150,
        top: 20,
        right: 110,
        bottom: 170,
        left: 10,
        toJSON: () => ({}),
      } as DOMRect;

      const el1 = document.createElement("div");
      el1.setAttribute("data-object-id", "42");
      el1.getBoundingClientRect = vi.fn(() => mockRect);

      const el2 = document.createElement("div");
      el2.setAttribute("data-object-id", "99");
      el2.getBoundingClientRect = vi.fn(() => mockRect);

      document.body.appendChild(el1);
      document.body.appendChild(el2);

      const snapshot = useAnimationStore.getState().captureSnapshot();

      expect(snapshot.size).toBe(2);
      expect(snapshot.get(42)).toBe(mockRect);
      expect(snapshot.get(99)).toBe(mockRect);

      document.body.removeChild(el1);
      document.body.removeChild(el2);
    });

    it("returns empty map when no elements exist", () => {
      const snapshot = useAnimationStore.getState().captureSnapshot();
      expect(snapshot.size).toBe(0);
    });
  });

  describe("positionRegistry", () => {
    it("registers and retrieves positions", () => {
      const rect = { x: 5, y: 10 } as DOMRect;
      useAnimationStore.getState().registerPosition(7, rect);

      expect(useAnimationStore.getState().getPosition(7)).toBe(rect);
    });
  });
});
