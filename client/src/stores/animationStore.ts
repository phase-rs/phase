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

/** A step as queued, stamped with the `EngineSnapshot.seq` of the state it
 *  animates: once `gameStore.lastCommittedSeq` reaches it, that state (or a
 *  newer one that superseded it) has committed. */
export interface QueuedStep extends AnimationStep {
  snapshotSeq: number;
  /** The Director `play()` run this step belongs to. Absent for a direct enqueue. */
  runId?: number;
}

interface AnimationStoreState {
  queue: QueuedStep[];
  activeStep: QueuedStep | null;
  activeGeneration: number;
  isPlaying: boolean;
  positionRegistry: Map<number, DOMRect>;
  animationNewState: GameState | null;
  displayedLife: DisplayedLifeTotals | null;
  /**
   * Objects the active step presents itself — e.g. the two cards a meld forge
   * lifts off the board — so their board cards hide until the step ends.
   * Step-scoped: every step change and queue clear resets it.
   */
  veiledObjectIds: ReadonlySet<number>;
  /**
   * Objects a card flight presents itself from launch to landing, so the
   * surfaces that render them hide until the flight releases them. Unlike
   * `veiledObjectIds`, a step change does not reset it, because a flight may
   * outlive its step. Only `clearQueue` resets it.
   */
  flightVeiledObjectIds: ReadonlySet<number>;
  /** Whether the card VFX layer is mounted and initialised, so it presents
   *  steps as GL effects (the New style); Classic presents them otherwise. */
  cardVfxReady: boolean;
  /**
   * The pace the Director last applied (speed preference × stack pressure), or
   * null when no run has been paced. Presentation reads this through
   * `usePresentationPace()`; the Director's clock reads it too.
   */
  pace: number | null;
  /**
   * The highest `snapshotSeq` handed to the presentation, kept after its steps
   * drain: a card flight may outlive its step and still owe its release to a
   * commit that is no longer queued here.
   */
  presentationSnapshotSeq: number;
  /**
   * The highest `snapshotSeq` whose commit this presentation will never see,
   * recorded when the dispatcher abandons in-flight work but keeps presenting
   * (a finished game). A veil hold at or below it settles on landing instead of
   * waiting for a commit that cannot come. Later snapshots carry higher stamps,
   * so a new session is unaffected.
   */
  abandonedSnapshotSeq: number;
}

interface AnimationStoreActions {
  enqueueSteps: (steps: readonly AnimationStep[], snapshotSeq: number, runId?: number) => void;
  advanceStep: () => void;
  /** Set the one applied pace; null restores the speed preference. */
  setPace: (pace: number | null) => void;
  captureSnapshot: () => PositionSnapshot;
  registerPosition: (objectId: number, rect: DOMRect) => void;
  getPosition: (objectId: number) => DOMRect | undefined;
  setAnimationNewState: (state: GameState | null) => void;
  /** Record an engine-reported life total whose hit has just landed on screen. */
  recordDisplayedLife: (playerId: number, life: number, engineCommitEpoch: number) => void;
  /** Hide these objects' board cards for the rest of the active step. */
  veilObjects: (objectIds: readonly number[]) => void;
  /** Hide this object's card surfaces until its flight releases it. */
  veilFlight: (objectId: number) => void;
  /** Release a flight veil. */
  unveilFlight: (objectId: number) => void;
  setCardVfxReady: (ready: boolean) => void;
  /** Record that no snapshot handed to the presentation so far will commit. */
  abandonPendingSnapshotCommits: () => void;
  /** Drop every step. `keepFlightVeils` leaves the veils to their owners, for a
   *  clear that leaves effects still drawing: each releases its own on landing. */
  clearQueue: (options?: { keepFlightVeils?: boolean }) => void;
}

export type AnimationStore = AnimationStoreState & AnimationStoreActions;

const NO_VEILED_OBJECTS: ReadonlySet<number> = new Set();

export const useAnimationStore = create<AnimationStore>()((set, get) => ({
  queue: [],
  activeStep: null,
  activeGeneration: 0,
  isPlaying: false,
  positionRegistry: new Map(),
  animationNewState: null,
  displayedLife: null,
  veiledObjectIds: NO_VEILED_OBJECTS,
  flightVeiledObjectIds: NO_VEILED_OBJECTS,
  cardVfxReady: false,
  pace: null,
  presentationSnapshotSeq: 0,
  abandonedSnapshotSeq: 0,

  enqueueSteps: (unstamped, snapshotSeq, runId) => {
    if (unstamped.length === 0) return;

    const steps = unstamped.map((step) => ({ ...step, snapshotSeq, runId }));
    const state = get();
    const { activeStep, queue } = state;
    // The newest snapshot the presentation has been handed, kept past the step
    // queue so a flight that outlives its step can still be settled on abandon.
    const presentationSnapshotSeq = Math.max(state.presentationSnapshotSeq, snapshotSeq);
    if (activeStep) {
      // Already animating — append to queue
      set({ queue: [...queue, ...steps], presentationSnapshotSeq });
    } else {
      // Nothing playing — promote first step immediately
      const [first, ...rest] = steps;
      set((state) => ({
        activeStep: first,
        activeGeneration: state.activeGeneration + 1,
        queue: rest,
        isPlaying: true,
        presentationSnapshotSeq,
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
        veiledObjectIds: NO_VEILED_OBJECTS,
      }));
    } else {
      set((state) => ({
        activeStep: null,
        activeGeneration: state.activeGeneration + 1,
        isPlaying: false,
        animationNewState: null,
        veiledObjectIds: NO_VEILED_OBJECTS,
        pace: null,
      }));
    }
  },

  setPace: (pace) => set({ pace }),

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

  veilObjects: (objectIds) => {
    set((state) => ({ veiledObjectIds: new Set([...state.veiledObjectIds, ...objectIds]) }));
  },

  veilFlight: (objectId) => {
    set((state) => ({ flightVeiledObjectIds: new Set([...state.flightVeiledObjectIds, objectId]) }));
  },

  unveilFlight: (objectId) => {
    set((state) => {
      const next = new Set(state.flightVeiledObjectIds);
      next.delete(objectId);
      return { flightVeiledObjectIds: next };
    });
  },

  setCardVfxReady: (ready) => set({ cardVfxReady: ready }),

  abandonPendingSnapshotCommits: () => set((state) => ({
    abandonedSnapshotSeq: Math.max(state.abandonedSnapshotSeq, state.presentationSnapshotSeq),
  })),

  clearQueue: (options) => set((state) => ({
    queue: [],
    activeStep: null,
    activeGeneration: state.activeGeneration + 1,
    isPlaying: false,
    animationNewState: null,
    displayedLife: null,
    veiledObjectIds: NO_VEILED_OBJECTS,
    flightVeiledObjectIds: options?.keepFlightVeils
      ? state.flightVeiledObjectIds
      : NO_VEILED_OBJECTS,
    pace: null,
  })),
}));
