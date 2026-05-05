import { create } from "zustand";

import {
  DraftAdapter,
  type DraftPlayerView,
  type SuggestedDeck,
} from "../adapter/draft-adapter";
import {
  clearActiveQuickDraft,
  clearQuickDraftSession,
  loadActiveQuickDraft,
  loadQuickDraftSession,
  saveActiveQuickDraft,
  saveQuickDraftSession,
} from "../services/quickDraftPersistence";

// ── Types ───────────────────────────────────────────────────────────────

export type DraftPhase = "setup" | "drafting" | "deckbuilding" | "launching";
export type PoolSortMode = "color" | "type" | "cmc";

interface DraftStoreState {
  draftId: string | null;
  adapter: DraftAdapter | null;
  view: DraftPlayerView | null;
  selectedCard: string | null;
  phase: DraftPhase;
  difficulty: number;
  selectedSet: string | null;
  mainDeck: string[];
  landCounts: Record<string, number>;
  poolSortMode: PoolSortMode;
  poolPanelOpen: boolean;
}

interface DraftStoreActions {
  startDraft: (setPoolJson: string, setCode: string, difficulty: number) => Promise<void>;
  resumeDraft: () => Promise<void>;
  abandonDraft: () => Promise<void>;
  pickCard: (cardInstanceId: string) => Promise<void>;
  selectCard: (cardInstanceId: string | null) => void;
  confirmPick: () => Promise<void>;
  addToDeck: (cardName: string) => void;
  removeFromDeck: (cardName: string) => void;
  setLandCount: (landName: string, count: number) => void;
  autoSuggestDeck: () => Promise<void>;
  autoSuggestLands: () => Promise<void>;
  submitDeck: () => Promise<void>;
  setPoolSortMode: (mode: PoolSortMode) => void;
  togglePoolPanel: () => void;
  setDifficulty: (d: number) => void;
  setSelectedSet: (s: string | null) => void;
  reset: () => void;
}

// ── Initial state ───────────────────────────────────────────────────────

const initialState: DraftStoreState = {
  draftId: null,
  adapter: null,
  view: null,
  selectedCard: null,
  phase: "setup",
  difficulty: 2,
  selectedSet: null,
  mainDeck: [],
  landCounts: {},
  poolSortMode: "color",
  poolPanelOpen: true,
};

// ── Persistence helpers ─────────────────────────────────────────────────

let debounceTimer: ReturnType<typeof setTimeout> | null = null;

function persistDraft(): void {
  const { adapter, draftId, phase, mainDeck, landCounts, poolSortMode, poolPanelOpen, difficulty, selectedSet } =
    useDraftStore.getState();
  if (!adapter || !draftId || !selectedSet || phase === "setup" || phase === "launching") return;
  const persistPhase = phase;

  void (async () => {
    try {
      const sessionJson = await adapter.exportSession();
      await saveQuickDraftSession(draftId, sessionJson, {
        phase: persistPhase,
        mainDeck,
        landCounts,
        poolSortMode,
        poolPanelOpen,
      });
      saveActiveQuickDraft({
        id: draftId,
        setCode: selectedSet,
        difficulty,
        phase: persistPhase,
        updatedAt: Date.now(),
      });
    } catch (err) {
      console.warn("[persistDraft] failed:", err);
    }
  })();
}

function persistDraftDebounced(): void {
  if (debounceTimer) clearTimeout(debounceTimer);
  debounceTimer = setTimeout(persistDraft, 500);
}

// ── Store ───────────────────────────────────────────────────────────────

export const useDraftStore = create<DraftStoreState & DraftStoreActions>()(
  (set, get) => ({
    ...initialState,

    startDraft: async (setPoolJson, setCode, difficulty) => {
      const adapter = new DraftAdapter();

      if (difficulty >= 3) {
        const resp = await fetch(__CARD_DATA_URL__);
        const json = await resp.text();
        await adapter.loadCardDatabase(json);
      }

      const seed = Math.floor(Math.random() * 0xffffffff);
      const view = await adapter.initialize(setPoolJson, difficulty, seed);
      const draftId = crypto.randomUUID();

      set({
        draftId,
        adapter,
        view,
        phase: "drafting",
        difficulty,
        selectedSet: setCode,
        selectedCard: null,
        mainDeck: [],
        landCounts: {},
      });

      persistDraft();
    },

    resumeDraft: async () => {
      const meta = loadActiveQuickDraft();
      if (!meta) return;

      const saved = await loadQuickDraftSession(meta.id);
      if (!saved) {
        await clearQuickDraftSession(meta.id);
        return;
      }

      try {
        const adapter = new DraftAdapter();

        if (meta.difficulty >= 3) {
          const resp = await fetch(__CARD_DATA_URL__);
          const json = await resp.text();
          await adapter.loadCardDatabase(json);
        }

        const view = await adapter.importSession(saved.sessionJson);

        set({
          draftId: meta.id,
          adapter,
          view,
          phase: meta.phase,
          difficulty: meta.difficulty,
          selectedSet: meta.setCode,
          selectedCard: null,
          mainDeck: saved.mainDeck,
          landCounts: saved.landCounts,
          poolSortMode: saved.poolSortMode,
          poolPanelOpen: saved.poolPanelOpen,
        });
      } catch (err) {
        console.warn("[resumeDraft] failed, clearing saved session:", err);
        await clearQuickDraftSession(meta.id);
        throw err;
      }
    },

    abandonDraft: async () => {
      const { draftId } = get();
      const id = draftId ?? loadActiveQuickDraft()?.id;
      if (id) {
        await clearQuickDraftSession(id);
      } else {
        clearActiveQuickDraft();
      }
      set(initialState);
    },

    pickCard: async (cardInstanceId) => {
      const { adapter } = get();
      if (!adapter) return;

      const view = await adapter.submitPick(cardInstanceId);
      const nextPhase: DraftPhase =
        view.status === "Deckbuilding" ? "deckbuilding" : "drafting";

      set({ view, phase: nextPhase, selectedCard: null });
      persistDraft();
    },

    selectCard: (cardInstanceId) => {
      set({ selectedCard: cardInstanceId });
    },

    confirmPick: async () => {
      const { selectedCard, pickCard } = get();
      if (!selectedCard) return;
      await pickCard(selectedCard);
    },

    addToDeck: (cardName) => {
      set((prev) => ({ mainDeck: [...prev.mainDeck, cardName] }));
      persistDraftDebounced();
    },

    removeFromDeck: (cardName) => {
      set((prev) => {
        const idx = prev.mainDeck.indexOf(cardName);
        if (idx === -1) return prev;
        const next = [...prev.mainDeck];
        next.splice(idx, 1);
        return { mainDeck: next };
      });
      persistDraftDebounced();
    },

    setLandCount: (landName, count) => {
      set((prev) => ({
        landCounts: { ...prev.landCounts, [landName]: Math.max(0, count) },
      }));
      persistDraftDebounced();
    },

    autoSuggestDeck: async () => {
      const { adapter } = get();
      if (!adapter) return;

      const result: SuggestedDeck = await adapter.suggestDeck();
      set({ mainDeck: result.main_deck, landCounts: result.lands });
      persistDraftDebounced();
    },

    autoSuggestLands: async () => {
      const { adapter, mainDeck } = get();
      if (!adapter) return;

      const lands = await adapter.suggestLands(mainDeck);
      set({ landCounts: lands });
      persistDraftDebounced();
    },

    submitDeck: async () => {
      const { adapter, mainDeck, landCounts, draftId } = get();
      if (!adapter) return;

      const landCards: string[] = [];
      for (const [name, count] of Object.entries(landCounts)) {
        for (let i = 0; i < count; i++) {
          landCards.push(name);
        }
      }

      const fullDeck = [...mainDeck, ...landCards];
      const view = await adapter.submitDeck(fullDeck);
      set({ view, phase: "launching" });

      if (draftId) void clearQuickDraftSession(draftId);
    },

    setPoolSortMode: (mode) => {
      set({ poolSortMode: mode });
      persistDraftDebounced();
    },

    togglePoolPanel: () => {
      set((prev) => ({ poolPanelOpen: !prev.poolPanelOpen }));
      persistDraftDebounced();
    },

    setDifficulty: (d) => {
      set({ difficulty: d });
    },

    setSelectedSet: (s) => {
      set({ selectedSet: s });
    },

    reset: () => {
      if (debounceTimer) clearTimeout(debounceTimer);
      set(initialState);
    },
  }),
);
