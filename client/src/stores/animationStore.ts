import { create } from "zustand";
import type { GameState } from "../adapter/types";
import type { AnimationStep, PositionSnapshot } from "../animation/types";

/**
 * Life totals to show while an animation window is open.
 *
 * The engine commits one state snapshot per action, only once every step of that
 * action has played, so a snapshot-only readout holds still through a whole
 * combat and then jumps. Each entry here is an engine-reported
 * `LifeChanged.new_total` whose hit has already landed on screen, which lets the
 * readout tick per hit while staying engine-authoritative — no amount is ever
 * accumulated client-side.
 */
interface DisplayedLifeTotals {
  /**
   * The `gameStore.engineCommitEpoch` these totals were recorded under. A newer
   * committed snapshot supersedes them with no clearing step and no race against
   * the animation's own timers — the same epoch guard the mana-payment preview
   * uses in `game/manaPaymentPreview.ts`.
   */
  epoch: number;
  totals: Map<number, number>;
}

interface AnimationStoreState {
  queue: AnimationStep[];
  activeStep: AnimationStep | null;
  activeGeneration: number;
  isPlaying: boolean;
  positionRegistry: Map<number, DOMRect>;
  animationNewState: GameState | null;
  displayedLife: DisplayedLifeTotals | null;
}

interface AnimationStoreActions {
  enqueueSteps: (steps: AnimationStep[]) => void;
  advanceStep: () => void;
  captureSnapshot: () => PositionSnapshot;
  registerPosition: (objectId: number, rect: DOMRect) => void;
  getPosition: (objectId: number) => DOMRect | undefined;
  setAnimationNewState: (state: GameState | null) => void;
  /** Record an engine-reported life total whose hit has just landed on screen. */
  recordDisplayedLife: (playerId: number, life: number, engineCommitEpoch: number) => void;
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
  displayedLife: null,

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

  recordDisplayedLife: (playerId, life, engineCommitEpoch) => {
    set((state) => {
      // A commit landed since the last record, so those totals describe an older
      // snapshot and must not be carried forward beside this one.
      const previous = state.displayedLife?.epoch === engineCommitEpoch
        ? state.displayedLife.totals
        : undefined;
      const totals = new Map(previous);
      totals.set(playerId, life);
      return { displayedLife: { epoch: engineCommitEpoch, totals } };
    });
  },

  clearQueue: () => set((state) => ({
    queue: [],
    activeStep: null,
    activeGeneration: state.activeGeneration + 1,
    isPlaying: false,
    animationNewState: null,
    displayedLife: null,
  })),
}));
