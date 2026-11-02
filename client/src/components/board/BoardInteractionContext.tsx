import { createContext, useContext } from "react";

interface BoardInteractionState {
  activatableObjectIds: Set<number>;
  /** Attackers this defender's blockers may legally block: the union of the
   *  current `DeclareBlockers` prompt's `valid_block_targets` values
   *  (CR 509.1a; already seat-scoped by
   *  `combat.rs::get_valid_block_targets_for_player`).
   *  Empty outside a `DeclareBlockers` prompt. */
  blockableAttackerIds: Set<number>;
  boardChoiceObjectIds: Set<number>;
  committedAttackerIds: Set<number>;
  /** Per-permanent count of attackers targeting it (Planeswalker / Battle
   *  attack targets). Computed once in GameBoard; each card reads O(1). */
  incomingAttackerCounts: ReadonlyMap<number, number>;
  manaTappableObjectIds: Set<number>;
  selectableSacrificeObjectIds: Set<number>;
  selectableManaCostCreatureIds: Set<number>;
  undoableTapObjectIds: Set<number>;
  validAttackerIds: Set<number>;
  validTargetObjectIds: Set<number>;
}

const EMPTY_SET = new Set<number>();
const EMPTY_MAP: ReadonlyMap<number, number> = new Map();

const EMPTY_STATE: BoardInteractionState = {
  activatableObjectIds: EMPTY_SET,
  blockableAttackerIds: EMPTY_SET,
  boardChoiceObjectIds: EMPTY_SET,
  committedAttackerIds: EMPTY_SET,
  incomingAttackerCounts: EMPTY_MAP,
  manaTappableObjectIds: EMPTY_SET,
  selectableSacrificeObjectIds: EMPTY_SET,
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
