import { createContext, useContext } from "react";

interface BoardInteractionState {
  activatableObjectIds: Set<number>;
  committedAttackerIds: Set<number>;
  /** Per-permanent count of attackers targeting it (Planeswalker / Battle
   *  attack targets). Computed once in GameBoard; each card reads O(1). */
  incomingAttackerCounts: ReadonlyMap<number, number>;
  manaTappableObjectIds: Set<number>;
  selectableManaCostCreatureIds: Set<number>;
  undoableTapObjectIds: Set<number>;
  validAttackerIds: Set<number>;
  validTargetObjectIds: Set<number>;
}

const EMPTY_SET = new Set<number>();
const EMPTY_MAP: ReadonlyMap<number, number> = new Map();

const EMPTY_STATE: BoardInteractionState = {
  activatableObjectIds: EMPTY_SET,
  committedAttackerIds: EMPTY_SET,
  incomingAttackerCounts: EMPTY_MAP,
  manaTappableObjectIds: EMPTY_SET,
  selectableManaCostCreatureIds: EMPTY_SET,
  undoableTapObjectIds: EMPTY_SET,
  validAttackerIds: EMPTY_SET,
  validTargetObjectIds: EMPTY_SET,
};

export const BoardInteractionContext =
  createContext<BoardInteractionState>(EMPTY_STATE);

export function useBoardInteractionState(): BoardInteractionState {
  return useContext(BoardInteractionContext);
}
