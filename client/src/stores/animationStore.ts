import { create } from "zustand";
import type { AnimationStep, PositionSnapshot } from "../animation/types";

interface AnimationStoreState {
  steps: AnimationStep[];
  isPlaying: boolean;
  positionRegistry: Map<number, DOMRect>;
}

interface AnimationStoreActions {
  enqueueSteps: (steps: AnimationStep[]) => void;
  playNextStep: () => AnimationStep | undefined;
  captureSnapshot: () => PositionSnapshot;
  registerPosition: (objectId: number, rect: DOMRect) => void;
  getPosition: (objectId: number) => DOMRect | undefined;
  clearQueue: () => void;
}

export type AnimationStore = AnimationStoreState & AnimationStoreActions;

export const useAnimationStore = create<AnimationStore>()((set, get) => ({
  steps: [],
  isPlaying: false,
  positionRegistry: new Map(),

  enqueueSteps: (steps) => {
    set((state) => ({
      steps: [...state.steps, ...steps],
      isPlaying: true,
    }));
  },

  playNextStep: () => {
    const { steps } = get();
    if (steps.length === 0) {
      set({ isPlaying: false });
      return undefined;
    }

    const [next, ...rest] = steps;
    set({ steps: rest, isPlaying: rest.length > 0 });
    return next;
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

  clearQueue: () => set({ steps: [], isPlaying: false }),
}));
