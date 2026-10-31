import { create } from "zustand";

import type { PlayerId } from "../adapter/types";

/**
 * The silly one-shot animations for Un-card actions (silver-border sets).
 * Each kind is a distinct gag; `text` carries the spoken phrase/word where the
 * gag shows one.
 */
export type UnFxKind =
  | "touchFace"
  | "touchTable"
  | "laugh"
  | "flickCards"
  | "saysWord"
  | "phrase"
  | "gotcha";

export interface UnFx {
  id: number;
  kind: UnFxKind;
  playerId: PlayerId;
  /** Spoken text (word, number, or phrase), shown inside the speech bubble. */
  text?: string;
}

interface UnActionFxStore {
  effects: UnFx[];
  push: (effect: Omit<UnFx, "id">) => void;
  remove: (id: number) => void;
  clear: () => void;
}

let nextId = 1;

export const useUnActionFxStore = create<UnActionFxStore>((set) => ({
  effects: [],
  push: (effect) =>
    set((state) => ({ effects: [...state.effects, { ...effect, id: nextId++ }] })),
  remove: (id) =>
    set((state) => ({ effects: state.effects.filter((fx) => fx.id !== id) })),
  clear: () => set({ effects: [] }),
}));
