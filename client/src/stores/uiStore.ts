import { create } from "zustand";
import type {
  GameAction,
  ObjectId,
} from "../adapter/types";
import { TURN_BANNER_DURATION_MS } from "../animation/types";
import { usePreferencesStore } from "./preferencesStore";

// Guard against spurious mouseleave events caused by Framer Motion layout
// recalculations or pointer-events-auto overlays stealing focus from the card.
// Clears are deferred — if the cursor is still over a card/preview element
// when the timer fires, the clear is suppressed.
let pendingClearTimer: ReturnType<typeof setTimeout> | null = null;
// Deferred-show timer for the configurable hover latency (cardPreviewHoverDelayMs).
// Holds the pending "set inspectedObjectId" so a hover-out before the delay
// elapses cancels it — the preview only appears once the cursor rests on a card.
let pendingShowTimer: ReturnType<typeof setTimeout> | null = null;
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
  /** Whether the Shift key is currently held. Drives the "shift" card-preview
   *  mode (preview shows only while Shift is down). Tracked as held-state via
   *  keydown/keyup (unlike altHeld, which press-toggles). */
  shiftHeld: boolean;
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
  turnBannerNumber: number | null;
  focusedOpponent: number | null;
  pendingAbilityChoice: { objectId: ObjectId; actions: GameAction[] } | null;
  /** When non-null, the AttachmentsDialog is open showing every Aura
   *  enchanting this player. Lives in uiStore (not local React state inside
   *  the badge) so the dialog can be rendered as a child of `<DialogHost>`
   *  — that's the only place where `fixed inset-0` dialog descendants
   *  reliably anchor to the viewport. Rendering from inside HudPlate would
   *  inherit Tailwind's `transform` containing block and shrink the
   *  dialog. See DialogHost.tsx:113-122 for the contract. */
  enchantmentsDialogPlayer: number | null;
  mobileHandOpen: boolean;
  debugPanelOpen: boolean;
  /** Which top-level tab the debug panel shows. Lifted out of DebugPanel's
   *  local state so entry points (Sandbox Tools nudge/button) can open the
   *  panel straight to "actions" instead of the default "console" log view. */
  debugPanelTab: "console" | "actions";
  debugInteractionMode: boolean;
  debugContextMenu: { objectId: ObjectId; x: number; y: number } | null;
  helpSheetOpen: boolean;
  /** Object currently being "previewed" by a debug-panel control (e.g. an
   *  ObjectSelect dropdown option under the cursor). Drives a distinct,
   *  always-obvious highlight on the board permanent / player avatar that is
   *  intentionally separate from `hoveredObjectId` — most board elements
   *  don't visibly react to plain hover, so a debug-panel preview needs its
   *  own loud signal. */
  debugHighlightedObjectId: ObjectId | null;
  debugHighlightedPlayerId: number | null;
  logPanelOpen: boolean;
}

interface UiStoreActions {
  selectObject: (id: ObjectId | null) => void;
  hoverObject: (id: ObjectId | null) => void;
  /** `timing` defaults to "hover" (subject to the configurable preview latency);
   *  "immediate" bypasses the delay for explicit-intent triggers (long-press). */
  inspectObject: (id: ObjectId | null, faceIndex?: number, timing?: "hover" | "immediate") => void;
  dismissPreview: () => void;
  setAltHeld: (held: boolean) => void;
  setShiftHeld: (held: boolean) => void;
  addSelectedCard: (cardId: ObjectId) => void;
  toggleSelectedCard: (cardId: ObjectId) => void;
  cycleSelectedCard: (cardId: ObjectId, max: number) => void;
  setGroupSelectedCards: (groupIds: ObjectId[], selectedIds: ObjectId[]) => void;
  clearSelectedCards: () => void;
  toggleFullControl: () => void;
  toggleAutoPass: () => void;
  setCombatMode: (mode: "attackers" | "blockers" | null) => void;
  toggleAttacker: (id: ObjectId) => void;
  setGroupSelectedAttackers: (groupIds: ObjectId[], selectedIds: ObjectId[]) => void;
  selectAllAttackers: (ids: ObjectId[]) => void;
  assignBlocker: (blockerId: ObjectId, attackerId: ObjectId) => void;
  removeBlockerAssignment: (blockerId: ObjectId) => void;
  clearCombatSelection: () => void;
  setCombatClickHandler: (handler: ((id: ObjectId) => void) | null) => void;
  setPreviewSticky: (sticky: boolean) => void;
  setDragging: (dragging: boolean) => void;
  flashTurnBanner: (text: string, turnNumber: number) => void;
  setFocusedOpponent: (id: number | null) => void;
  setPendingAbilityChoice: (choice: { objectId: ObjectId; actions: GameAction[] } | null) => void;
  setEnchantmentsDialogPlayer: (id: number | null) => void;
  setMobileHandOpen: (open: boolean) => void;
  toggleDebugPanel: () => void;
  setDebugPanelTab: (tab: "console" | "actions") => void;
  /** Open the debug panel directly to the Actions ("Sandbox Tools") tab. */
  openSandboxTools: () => void;
  toggleDebugInteractionMode: () => void;
  openDebugContextMenu: (menu: { objectId: ObjectId; x: number; y: number }) => void;
  closeDebugContextMenu: () => void;
  setHelpSheetOpen: (open: boolean) => void;
  toggleHelpSheet: () => void;
  /** Set or clear the debug-panel preview highlight for an object. */
  setDebugHighlightedObjectId: (id: ObjectId | null) => void;
  /** Set or clear the debug-panel preview highlight for a player. */
  setDebugHighlightedPlayerId: (id: number | null) => void;
  setLogPanelOpen: (open: boolean) => void;
  toggleLogPanel: () => void;
}

export type UiStore = UiStoreState & UiStoreActions;

export const useUiStore = create<UiStore>()((set, get) => ({
  selectedObjectId: null,
  hoveredObjectId: null,
  inspectedObjectId: null,
  inspectedFaceIndex: 0,
  altHeld: false,
  shiftHeld: false,
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
  turnBannerNumber: null,
  focusedOpponent: null,
  pendingAbilityChoice: null,
  enchantmentsDialogPlayer: null,
  mobileHandOpen: false,
  debugPanelOpen: false,
  debugPanelTab: "console",
  debugInteractionMode: false,
  debugContextMenu: null,
  helpSheetOpen: false,
  debugHighlightedObjectId: null,
  debugHighlightedPlayerId: null,
  logPanelOpen: false,

  selectObject: (id) => set({ selectedObjectId: id }),
  hoverObject: (id) => set({ hoveredObjectId: id }),
  setDebugHighlightedObjectId: (id) => set({ debugHighlightedObjectId: id }),
  setDebugHighlightedPlayerId: (id) => set({ debugHighlightedPlayerId: id }),
  setAltHeld: (held) => set({ altHeld: held }),
  setShiftHeld: (held) => set({ shiftHeld: held }),
  inspectObject: (id, faceIndex, timing = "hover") => {
    if (id != null) {
      // Setting a new inspection target: cancel any pending clear, and drop a
      // pending delayed-show for a previous target before scheduling this one.
      if (pendingClearTimer != null) {
        clearTimeout(pendingClearTimer);
        pendingClearTimer = null;
      }
      if (pendingShowTimer != null) {
        clearTimeout(pendingShowTimer);
        pendingShowTimer = null;
      }
      const applyInspect = () =>
        set({ inspectedObjectId: id, inspectedFaceIndex: faceIndex ?? 0 });
      // Configurable hover latency (cardPreviewHoverDelayMs). The delay gates only
      // the FIRST appearance on a hover-capable device: while a preview is already
      // open, sweeping to an adjacent card switches instantly, and the "shift"
      // bind-key mode is keypress-triggered so it never waits (mutually exclusive
      // with the latency). A 0ms delay (the default) keeps the prior instant feel.
      const prefs = usePreferencesStore.getState();
      const canHover =
        typeof window !== "undefined" &&
        typeof window.matchMedia === "function" &&
        window.matchMedia("(hover: hover)").matches;
      const delay =
        canHover &&
        timing !== "immediate" &&
        prefs.cardPreviewMode !== "shift" &&
        get().inspectedObjectId == null
          ? prefs.cardPreviewHoverDelayMs
          : 0;
      if (delay > 0) {
        pendingShowTimer = setTimeout(() => {
          pendingShowTimer = null;
          applyInspect();
        }, delay);
      } else {
        applyInspect();
      }
    } else {
      // Clearing: drop any pending delayed-show so a hover-out before the latency
      // elapses never pops the preview.
      if (pendingShowTimer != null) {
        clearTimeout(pendingShowTimer);
        pendingShowTimer = null;
      }
      // Defer the clear so spurious mouseleave from re-render-induced layout shifts
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

  dismissPreview: () => {
    if (pendingClearTimer != null) {
      clearTimeout(pendingClearTimer);
      pendingClearTimer = null;
    }
    if (pendingShowTimer != null) {
      clearTimeout(pendingShowTimer);
      pendingShowTimer = null;
    }
    set({ inspectedObjectId: null, inspectedFaceIndex: 0, previewSticky: false, altHeld: false });
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

  // Capped multi-select for "choose exactly N" prompts (e.g. London mulligan
  // bottoming). Clicking a selected card deselects it; clicking an unselected
  // card adds it while under `max`; clicking an unselected card at `max` evicts
  // the oldest selection so the click swaps the choice instead of being ignored
  // (a straight swap when max === 1).
  cycleSelectedCard: (cardId, max) =>
    set((state) => {
      const selected = state.selectedCardIds;
      if (selected.includes(cardId)) {
        return { selectedCardIds: selected.filter((id) => id !== cardId) };
      }
      if (selected.length < max) {
        return { selectedCardIds: [...selected, cardId] };
      }
      return { selectedCardIds: [...selected.slice(1), cardId] };
    }),

  setGroupSelectedCards: (groupIds, selectedIds) =>
    set((state) => {
      const groupIdSet = new Set(groupIds);
      return {
        selectedCardIds: [
          ...state.selectedCardIds.filter((id) => !groupIdSet.has(id)),
          ...selectedIds,
        ],
      };
    }),

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

  setGroupSelectedAttackers: (groupIds, selectedIds) =>
    set((state) => {
      const groupIdSet = new Set(groupIds);
      return {
        selectedAttackers: [
          ...state.selectedAttackers.filter((id) => !groupIdSet.has(id)),
          ...selectedIds,
        ],
      };
    }),

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
  flashTurnBanner: (text, turnNumber) => {
    // Banner duration scales with both the global Animation Speed slider
    // (animationSpeedMultiplier) and the per-category Banner Pacing slider
    // (pacingMultipliers.banners). When animationSpeedMultiplier is 0
    // ("instant"), skip the banner entirely so it never lingers.
    const prefs = usePreferencesStore.getState();
    const speed = prefs.animationSpeedMultiplier;
    if (speed <= 0) return;
    const banner = prefs.pacingMultipliers.banners;
    const duration = TURN_BANNER_DURATION_MS * speed * banner;
    set({ showTurnBanner: true, turnBannerText: text, turnBannerNumber: turnNumber });
    setTimeout(() => set({ showTurnBanner: false }), duration);
  },
  setFocusedOpponent: (id) => set({ focusedOpponent: id }),
  setPendingAbilityChoice: (choice) => set({ pendingAbilityChoice: choice }),
  setEnchantmentsDialogPlayer: (id) => set({ enchantmentsDialogPlayer: id }),
  setMobileHandOpen: (open) => set({ mobileHandOpen: open }),
  toggleDebugPanel: () => set((state) => ({ debugPanelOpen: !state.debugPanelOpen })),
  setDebugPanelTab: (tab) => set({ debugPanelTab: tab }),
  openSandboxTools: () => set({ debugPanelOpen: true, debugPanelTab: "actions" }),
  toggleDebugInteractionMode: () => set((state) => ({
    debugInteractionMode: !state.debugInteractionMode,
    debugContextMenu: null,
  })),
  openDebugContextMenu: (menu) => set({ debugContextMenu: menu, selectedObjectId: menu.objectId }),
  closeDebugContextMenu: () => set({ debugContextMenu: null }),
  setHelpSheetOpen: (open) => set({ helpSheetOpen: open }),
  toggleHelpSheet: () => set((state) => ({ helpSheetOpen: !state.helpSheetOpen })),
  setLogPanelOpen: (open) => set({ logPanelOpen: open }),
  toggleLogPanel: () => set((state) => ({ logPanelOpen: !state.logPanelOpen })),
}));
