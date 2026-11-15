import { create } from "zustand";

import type { AttentionCue } from "../game/priorityAttention";

/**
 * Live output of the priority-attention controller
 * (`usePriorityAttentionController`). Every cue — pill/button pulse, chime,
 * tab title — reads this one store, so they cannot disagree about whether an
 * announcement is pending and a component mounted several times (the turn
 * status pill is) never double-fires a side effect.
 */
interface PriorityAttentionState {
  /** What is currently shown: nothing, an announced window, or its stronger reminder. */
  cue: AttentionCue;
  /** Increments once per chime and never goes back down while the page lives,
   *  across session replacements and remounts. Effects key on a rise in this,
   *  not on `cue`, so back-to-back chimes each sound. */
  chimeCount: number;
  publish: (next: Pick<PriorityAttentionState, "cue" | "chimeCount">) => void;
}

export const usePriorityAttentionStore = create<PriorityAttentionState>((set) => ({
  cue: "none",
  chimeCount: 0,
  publish: (next) => set(next),
}));
