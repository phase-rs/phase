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
      useAnimationStore.getState().enqueueSteps(steps);

      const state = useAnimationStore.getState();
      expect(state.activeStep).toEqual(steps[0]);
      expect(state.queue).toHaveLength(1);
      expect(state.isPlaying).toBe(true);
    });

    it("appends to queue when already playing", () => {
      useAnimationStore.getState().enqueueSteps([makeStep()]);
      useAnimationStore.getState().enqueueSteps([makeStep(), makeStep()]);

      const state = useAnimationStore.getState();
      expect(state.activeStep).toBeTruthy();
      expect(state.queue).toHaveLength(2);
    });
  });

  describe("advanceStep", () => {
    it("advances through steps in order", () => {
      const step1 = makeStep(100);
      const step2 = makeStep(200);
      useAnimationStore.getState().enqueueSteps([step1, step2]);

      expect(useAnimationStore.getState().activeStep).toEqual(step1);
      expect(useAnimationStore.getState().isPlaying).toBe(true);

      useAnimationStore.getState().advanceStep();
      expect(useAnimationStore.getState().activeStep).toEqual(step2);
      expect(useAnimationStore.getState().isPlaying).toBe(true);

      useAnimationStore.getState().advanceStep();
      expect(useAnimationStore.getState().activeStep).toBeNull();
      expect(useAnimationStore.getState().isPlaying).toBe(false);
    });

    it("clears when queue is empty", () => {
      useAnimationStore.getState().enqueueSteps([makeStep()]);
      useAnimationStore.getState().advanceStep();

      const state = useAnimationStore.getState();
      expect(state.activeStep).toBeNull();
      expect(state.isPlaying).toBe(false);
    });
  });

  describe("clearQueue", () => {
    it("resets all animation state", () => {
      useAnimationStore.getState().enqueueSteps([makeStep(), makeStep()]);
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
