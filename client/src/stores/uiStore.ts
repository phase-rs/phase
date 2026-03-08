import { create } from "zustand";
import type { ObjectId } from "../adapter/types";

interface UiStoreState {
  selectedObjectId: ObjectId | null;
  hoveredObjectId: ObjectId | null;
  inspectedObjectId: ObjectId | null;
  targetingMode: boolean;
  selectedTargets: ObjectId[];
  fullControl: boolean;
  autoPass: boolean;
}

interface UiStoreActions {
  selectObject: (id: ObjectId | null) => void;
  hoverObject: (id: ObjectId | null) => void;
  inspectObject: (id: ObjectId | null) => void;
  startTargeting: () => void;
  addTarget: (id: ObjectId) => void;
  clearTargets: () => void;
  toggleFullControl: () => void;
  toggleAutoPass: () => void;
}

export type UiStore = UiStoreState & UiStoreActions;

export const useUiStore = create<UiStore>()((set) => ({
  selectedObjectId: null,
  hoveredObjectId: null,
  inspectedObjectId: null,
  targetingMode: false,
  selectedTargets: [],
  fullControl: false,
  autoPass: false,

  selectObject: (id) => set({ selectedObjectId: id }),
  hoverObject: (id) => set({ hoveredObjectId: id }),
  inspectObject: (id) => set({ inspectedObjectId: id }),

  startTargeting: () => set({ targetingMode: true, selectedTargets: [] }),

  addTarget: (id) =>
    set((state) => ({
      selectedTargets: [...state.selectedTargets, id],
    })),

  clearTargets: () =>
    set({ targetingMode: false, selectedTargets: [] }),

  toggleFullControl: () =>
    set((state) => ({ fullControl: !state.fullControl })),

  toggleAutoPass: () =>
    set((state) => ({ autoPass: !state.autoPass })),
}));
