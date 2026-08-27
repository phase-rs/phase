import { create } from "zustand";
import type { GameState } from "../adapter/types";
import type { AnimationStep, PositionSnapshot } from "../animation/types";

interface AnimationStoreState {
  queue: AnimationStep[];
  activeStep: AnimationStep | null;
  activeGeneration: number;
  isPlaying: boolean;
  positionRegistry: Map<number, DOMRect>;
  animationNewState: GameState | null;
}

interface AnimationStoreActions {
  enqueueSteps: (steps: AnimationStep[]) => void;
  advanceStep: () => void;
  captureSnapshot: () => PositionSnapshot;
  registerPosition: (objectId: number, rect: DOMRect) => void;
  getPosition: (objectId: number) => DOMRect | undefined;
  setAnimationNewState: (state: GameState | null) => void;
  clearQueue: () => void;
}

export type AnimationStore = AnimationStoreState & AnimationStoreActions;

export const useAnimationStore = create<AnimationStore>()((set, get) => ({
  queue: [],
  activeStep: null,
  activeGeneration: 0,
  isPlaying: false,
  positionRegistry: new Map(),
  animationNewState: null,

  enqueueSteps: (steps) => {
    if (steps.length === 0) return;

    const { activeStep, queue } = get();
    if (activeStep) {
      // Already animating — append to queue
      set({ queue: [...queue, ...steps] });
    } else {
      // Nothing playing — promote first step immediately
      const [first, ...rest] = steps;
      set((state) => ({
        activeStep: first,
        activeGeneration: state.activeGeneration + 1,
        queue: rest,
        isPlaying: true,
      }));
    }
  },

  advanceStep: () => {
    const { queue } = get();
    if (queue.length > 0) {
      const [next, ...rest] = queue;
      set((state) => ({
        activeStep: next,
        activeGeneration: state.activeGeneration + 1,
        queue: rest,
      }));
    } else {
      set((state) => ({
        activeStep: null,
        activeGeneration: state.activeGeneration + 1,
        isPlaying: false,
        animationNewState: null,
      }));
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

  setAnimationNewState: (state) => set({ animationNewState: state }),

  clearQueue: () => set((state) => ({
    queue: [],
    activeStep: null,
    activeGeneration: state.activeGeneration + 1,
    isPlaying: false,
    animationNewState: null,
  })),
}));
