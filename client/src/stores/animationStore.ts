import { create } from "zustand";
import type { AnimationStep, PositionSnapshot } from "../animation/types";

interface AnimationStoreState {
  queue: AnimationStep[];
  activeStep: AnimationStep | null;
  isPlaying: boolean;
  positionRegistry: Map<number, DOMRect>;
}

interface AnimationStoreActions {
  enqueueSteps: (steps: AnimationStep[]) => void;
  advanceStep: () => void;
  captureSnapshot: () => PositionSnapshot;
  registerPosition: (objectId: number, rect: DOMRect) => void;
  getPosition: (objectId: number) => DOMRect | undefined;
  clearQueue: () => void;
}

export type AnimationStore = AnimationStoreState & AnimationStoreActions;

export const useAnimationStore = create<AnimationStore>()((set, get) => ({
  queue: [],
  activeStep: null,
  isPlaying: false,
  positionRegistry: new Map(),

  enqueueSteps: (steps) => {
    if (steps.length === 0) return;

    const { activeStep, queue } = get();
    if (activeStep) {
      // Already animating — append to queue
      set({ queue: [...queue, ...steps] });
    } else {
      // Nothing playing — promote first step immediately
      const [first, ...rest] = steps;
      set({ activeStep: first, queue: rest, isPlaying: true });
    }
  },

  advanceStep: () => {
    const { queue } = get();
    if (queue.length > 0) {
      const [next, ...rest] = queue;
      set({ activeStep: next, queue: rest });
    } else {
      set({ activeStep: null, isPlaying: false });
    }
  },

  captureSnapshot: () => {
    const snapshot: PositionSnapshot = new Map();
    const elements = document.querySelectorAll("[data-object-id]");
    for (const el of elements) {
      const id = Number(el.getAttribute("data-object-id"));
      if (!Number.isNaN(id)) {
        snapshot.set(id, el.getBoundingClientRect());
      }
    }
    return snapshot;
  },

  registerPosition: (objectId, rect) => {
    set((state) => {
      const newRegistry = new Map(state.positionRegistry);
      newRegistry.set(objectId, rect);
      return { positionRegistry: newRegistry };
    });
  },

  getPosition: (objectId) => get().positionRegistry.get(objectId),

  clearQueue: () => set({ queue: [], activeStep: null, isPlaying: false }),
}));
