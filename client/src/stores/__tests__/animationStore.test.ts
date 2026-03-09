import { describe, it, expect, beforeEach, vi } from "vitest";
import { useAnimationStore } from "../animationStore";
import type { AnimationStep } from "../../animation/types";

describe("animationStore", () => {
  beforeEach(() => {
    useAnimationStore.getState().clearQueue();
  });

  const makeStep = (duration = 300): AnimationStep => ({
    effects: [{ type: "DamageDealt", data: { amount: 3 }, duration }],
    duration,
  });

  describe("enqueueSteps", () => {
    it("adds steps to the queue and sets isPlaying", () => {
      const steps = [makeStep(300), makeStep(400)];
      useAnimationStore.getState().enqueueSteps(steps);

      const state = useAnimationStore.getState();
      expect(state.steps).toHaveLength(2);
      expect(state.isPlaying).toBe(true);
    });

    it("appends to existing steps", () => {
      useAnimationStore.getState().enqueueSteps([makeStep()]);
      useAnimationStore.getState().enqueueSteps([makeStep(), makeStep()]);

      expect(useAnimationStore.getState().steps).toHaveLength(3);
    });
  });

  describe("playNextStep", () => {
    it("returns steps in order", () => {
      const step1 = makeStep(100);
      const step2 = makeStep(200);
      useAnimationStore.getState().enqueueSteps([step1, step2]);

      const first = useAnimationStore.getState().playNextStep();
      expect(first).toEqual(step1);
      expect(useAnimationStore.getState().steps).toHaveLength(1);
      expect(useAnimationStore.getState().isPlaying).toBe(true);

      const second = useAnimationStore.getState().playNextStep();
      expect(second).toEqual(step2);
      expect(useAnimationStore.getState().steps).toHaveLength(0);
      expect(useAnimationStore.getState().isPlaying).toBe(false);
    });

    it("returns undefined when queue is empty", () => {
      const result = useAnimationStore.getState().playNextStep();
      expect(result).toBeUndefined();
      expect(useAnimationStore.getState().isPlaying).toBe(false);
    });
  });

  describe("clearQueue", () => {
    it("resets steps and isPlaying", () => {
      useAnimationStore.getState().enqueueSteps([makeStep(), makeStep()]);
      expect(useAnimationStore.getState().isPlaying).toBe(true);

      useAnimationStore.getState().clearQueue();

      const state = useAnimationStore.getState();
      expect(state.steps).toHaveLength(0);
      expect(state.isPlaying).toBe(false);
    });
  });

  describe("captureSnapshot", () => {
    it("reads data-object-id elements from DOM", () => {
      // Mock DOM elements with data-object-id
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
