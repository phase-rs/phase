import { create } from "zustand";
import type {
  GameAction,
  ObjectId,
} from "../adapter/types";

// Guard against spurious mouseleave events caused by Framer Motion layout
// recalculations or pointer-events-auto overlays stealing focus from the card.
// Clears are deferred — if the cursor is still over a card/preview element
// when the timer fires, the clear is suppressed.
let pendingClearTimer: ReturnType<typeof setTimeout> | null = null;
let lastPointer = { x: 0, y: 0 };
if (typeof window !== "undefined") {
  window.addEventListener("pointermove", (e) => { lastPointer = { x: e.clientX, y: e.clientY }; }, { passive: true });
}

interface UiStoreState {
  selectedObjectId: ObjectId | null;
  hoveredObjectId: ObjectId | null;
  inspectedObjectId: ObjectId | null;
  inspectedFaceIndex: number;
  altHeld: boolean;
  selectedCardIds: ObjectId[];
  fullControl: boolean;
  autoPass: boolean;
  combatMode: "attackers" | "blockers" | null;
  selectedAttackers: ObjectId[];
  blockerAssignments: Map<ObjectId, ObjectId>;
  combatClickHandler: ((id: ObjectId) => void) | null;
  previewSticky: boolean;
  isDragging: boolean;
  showTurnBanner: boolean;
  turnBannerText: string;
  focusedOpponent: number | null;
  pendingAbilityChoice: { objectId: ObjectId; actions: GameAction[] } | null;
  debugPanelOpen: boolean;
  logPanelOpen: boolean;
}

interface UiStoreActions {
  selectObject: (id: ObjectId | null) => void;
  hoverObject: (id: ObjectId | null) => void;
  inspectObject: (id: ObjectId | null, faceIndex?: number) => void;
  setAltHeld: (held: boolean) => void;
  addSelectedCard: (cardId: ObjectId) => void;
  toggleSelectedCard: (cardId: ObjectId) => void;
  clearSelectedCards: () => void;
  toggleFullControl: () => void;
  toggleAutoPass: () => void;
  setCombatMode: (mode: "attackers" | "blockers" | null) => void;
  toggleAttacker: (id: ObjectId) => void;
  selectAllAttackers: (ids: ObjectId[]) => void;
  assignBlocker: (blockerId: ObjectId, attackerId: ObjectId) => void;
  removeBlockerAssignment: (blockerId: ObjectId) => void;
  clearCombatSelection: () => void;
  setCombatClickHandler: (handler: ((id: ObjectId) => void) | null) => void;
  setPreviewSticky: (sticky: boolean) => void;
  setDragging: (dragging: boolean) => void;
  flashTurnBanner: (text: string) => void;
  setFocusedOpponent: (id: number | null) => void;
  setPendingAbilityChoice: (choice: { objectId: ObjectId; actions: GameAction[] } | null) => void;
  toggleDebugPanel: () => void;
  setLogPanelOpen: (open: boolean) => void;
  toggleLogPanel: () => void;
}

export type UiStore = UiStoreState & UiStoreActions;

export const useUiStore = create<UiStore>()((set) => ({
  selectedObjectId: null,
  hoveredObjectId: null,
  inspectedObjectId: null,
  inspectedFaceIndex: 0,
  altHeld: false,
  selectedCardIds: [],
  fullControl: false,
  autoPass: false,
  combatMode: null,
  selectedAttackers: [],
  blockerAssignments: new Map(),
  combatClickHandler: null,
  previewSticky: false,
  isDragging: false,
  showTurnBanner: false,
  turnBannerText: "",
  focusedOpponent: null,
  pendingAbilityChoice: null,
  debugPanelOpen: false,
  logPanelOpen: false,

  selectObject: (id) => set({ selectedObjectId: id }),
  hoverObject: (id) => set({ hoveredObjectId: id }),
  setAltHeld: (held) => set({ altHeld: held }),
  inspectObject: (id, faceIndex) => {
    if (id != null) {
      // Setting a new inspection target: cancel any pending clear and apply immediately
      if (pendingClearTimer != null) {
        clearTimeout(pendingClearTimer);
        pendingClearTimer = null;
      }
      set({ inspectedObjectId: id, inspectedFaceIndex: faceIndex ?? 0 });
    } else {
      // Clearing: defer so spurious mouseleave from re-render-induced layout shifts
      // is cancelled if a new inspectObject(id) arrives in the same frame.
      if (pendingClearTimer != null) return; // already scheduled
      pendingClearTimer = setTimeout(() => {
        pendingClearTimer = null;
        // Suppress clear only if cursor is over the preview panel itself, so Alt-mode
        // reading of the parsed abilities panel isn't dismissed when mousing onto it.
        // We intentionally do NOT suppress when cursor is over another card-hover: the
        // next card's onMouseEnter already cancels this timer via the id != null branch.
        const el = document.elementFromPoint(lastPointer.x, lastPointer.y);
        if (el?.closest("[data-card-preview]")) return;
        set({ inspectedObjectId: null, inspectedFaceIndex: 0, previewSticky: false, altHeld: false });
      }, 50);
    }
  },

  addSelectedCard: (cardId) =>
    set((state) => ({
      selectedCardIds: [...state.selectedCardIds, cardId],
    })),

  toggleSelectedCard: (cardId) =>
    set((state) => ({
      selectedCardIds: state.selectedCardIds.includes(cardId)
        ? state.selectedCardIds.filter((id) => id !== cardId)
        : [...state.selectedCardIds, cardId],
    })),

  clearSelectedCards: () =>
    set({
      selectedCardIds: [],
    }),

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
  setPreviewSticky: (sticky) => set({ previewSticky: sticky }),
  setDragging: (dragging) => set({ isDragging: dragging }),
  flashTurnBanner: (text) => {
    set({ showTurnBanner: true, turnBannerText: text });
    setTimeout(() => set({ showTurnBanner: false }), 1500);
  },
  setFocusedOpponent: (id) => set({ focusedOpponent: id }),
  setPendingAbilityChoice: (choice) => set({ pendingAbilityChoice: choice }),
  toggleDebugPanel: () => set((state) => ({ debugPanelOpen: !state.debugPanelOpen })),
  setLogPanelOpen: (open) => set({ logPanelOpen: open }),
  toggleLogPanel: () => set((state) => ({ logPanelOpen: !state.logPanelOpen })),
}));
