import { create } from "zustand";
import type { ObjectId } from "../adapter/types";

interface UiStoreState {
  selectedObjectId: ObjectId | null;
  hoveredObjectId: ObjectId | null;
  inspectedObjectId: ObjectId | null;
  inspectedFaceIndex: number;
  targetingMode: boolean;
  validTargetIds: ObjectId[];
  sourceObjectId: ObjectId | null;
  selectedTargets: ObjectId[];
  fullControl: boolean;
  autoPass: boolean;
  combatMode: "attackers" | "blockers" | null;
  selectedAttackers: ObjectId[];
  blockerAssignments: Map<ObjectId, ObjectId>;
  combatClickHandler: ((id: ObjectId) => void) | null;
  isDragging: boolean;
  endTurnMode: boolean;
  endTurnSinceTurn: number | null;
}

interface UiStoreActions {
  selectObject: (id: ObjectId | null) => void;
  hoverObject: (id: ObjectId | null) => void;
  inspectObject: (id: ObjectId | null, faceIndex?: number) => void;
  startTargeting: (validIds: ObjectId[], sourceId: ObjectId | null) => void;
  addTarget: (id: ObjectId) => void;
  clearTargets: () => void;
  toggleFullControl: () => void;
  toggleAutoPass: () => void;
  setCombatMode: (mode: "attackers" | "blockers" | null) => void;
  toggleAttacker: (id: ObjectId) => void;
  selectAllAttackers: (ids: ObjectId[]) => void;
  assignBlocker: (blockerId: ObjectId, attackerId: ObjectId) => void;
  removeBlockerAssignment: (blockerId: ObjectId) => void;
  clearCombatSelection: () => void;
  setCombatClickHandler: (handler: ((id: ObjectId) => void) | null) => void;
  setDragging: (dragging: boolean) => void;
  toggleEndTurnMode: (turnNumber: number) => void;
  clearEndTurnMode: () => void;
}

export type UiStore = UiStoreState & UiStoreActions;

export const useUiStore = create<UiStore>()((set) => ({
  selectedObjectId: null,
  hoveredObjectId: null,
  inspectedObjectId: null,
  inspectedFaceIndex: 0,
  targetingMode: false,
  validTargetIds: [],
  sourceObjectId: null,
  selectedTargets: [],
  fullControl: false,
  autoPass: false,
  combatMode: null,
  selectedAttackers: [],
  blockerAssignments: new Map(),
  combatClickHandler: null,
  isDragging: false,
  endTurnMode: false,
  endTurnSinceTurn: null,

  selectObject: (id) => set({ selectedObjectId: id }),
  hoverObject: (id) => set({ hoveredObjectId: id }),
  inspectObject: (id, faceIndex) => set({ inspectedObjectId: id, inspectedFaceIndex: faceIndex ?? 0 }),

  startTargeting: (validIds, sourceId) =>
    set({ targetingMode: true, validTargetIds: validIds, sourceObjectId: sourceId, selectedTargets: [] }),

  addTarget: (id) =>
    set((state) => ({
      selectedTargets: [...state.selectedTargets, id],
    })),

  clearTargets: () =>
    set({ targetingMode: false, validTargetIds: [], sourceObjectId: null, selectedTargets: [] }),

  toggleFullControl: () =>
    set((state) => ({ fullControl: !state.fullControl })),

  toggleAutoPass: () =>
    set((state) => ({ autoPass: !state.autoPass })),

  setCombatMode: (mode) => set({ combatMode: mode }),

  toggleAttacker: (id) =>
    set((state) => ({
      selectedAttackers: state.selectedAttackers.includes(id)
        ? state.selectedAttackers.filter((a) => a !== id)
        : [...state.selectedAttackers, id],
    })),

  selectAllAttackers: (ids) => set({ selectedAttackers: ids }),

  assignBlocker: (blockerId, attackerId) =>
    set((state) => {
      const next = new Map(state.blockerAssignments);
      next.set(blockerId, attackerId);
      return { blockerAssignments: next };
    }),

  removeBlockerAssignment: (blockerId) =>
    set((state) => {
      const next = new Map(state.blockerAssignments);
      next.delete(blockerId);
      return { blockerAssignments: next };
    }),

  clearCombatSelection: () =>
    set({
      combatMode: null,
      selectedAttackers: [],
      blockerAssignments: new Map(),
      combatClickHandler: null,
    }),

  setCombatClickHandler: (handler) => set({ combatClickHandler: handler }),
  setDragging: (dragging) => set({ isDragging: dragging }),
  toggleEndTurnMode: (turnNumber) =>
    set((state) => ({
      endTurnMode: !state.endTurnMode,
      endTurnSinceTurn: !state.endTurnMode ? turnNumber : null,
    })),
  clearEndTurnMode: () => set({ endTurnMode: false, endTurnSinceTurn: null }),
}));
