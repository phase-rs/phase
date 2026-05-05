import { create } from "zustand";

import {
  DraftAdapter,
  type DraftPlayerView,
  type SuggestedDeck,
} from "../adapter/draft-adapter";
import { useGameStore } from "./gameStore";
import {
  clearActiveQuickDraft,
  clearDraftRun,
  clearQuickDraftSession,
  loadActiveQuickDraft,
  loadDraftRun,
  loadQuickDraftSession,
  runLimits,
  saveActiveQuickDraft,
  saveDraftRun,
  saveQuickDraftSession,
} from "../services/quickDraftPersistence";
import type {
  DraftMatchResult,
  DraftRunFormat,
  DraftRunState,
} from "../services/quickDraftPersistence";

// ── Types ───────────────────────────────────────────────────────────────

export type DraftPhase = "setup" | "drafting" | "deckbuilding" | "launching" | "playing" | "complete";
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
  runFormat: DraftRunFormat;
  runState: DraftRunState | null;
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
  setRunFormat: (f: DraftRunFormat) => void;
  launchMatch: (navigate: (path: string) => void) => Promise<void>;
  recordMatchResult: (gameId: string, result: DraftMatchResult) => Promise<void>;
  launchNextMatch: (navigate: (path: string) => void) => Promise<void>;
  endRun: () => Promise<void>;
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
  runFormat: "run",
  runState: null,
};

// ── Constants ──────────────────────────────────────────────────────────

const DIFFICULTY_NAMES = ["VeryEasy", "Easy", "Medium", "Hard", "VeryHard"] as const;
const DRAFT_DECK_SESSION_KEY = "phase:draft-deck";

// ── Match helpers ──────────────────────────────────────────────────────

function expandLands(landCounts: Record<string, number>): string[] {
  const cards: string[] = [];
  for (const [name, count] of Object.entries(landCounts)) {
    for (let i = 0; i < count; i++) {
      cards.push(name);
    }
  }
  return cards;
}

function storeDraftDeckData(
  gameId: string,
  playerDeck: string[],
  opponentDeck: string[],
): void {
  const data = {
    player: { main_deck: playerDeck, sideboard: [], commander: [] },
    opponent: { main_deck: opponentDeck, sideboard: [], commander: [] },
    ai_decks: [],
  };
  sessionStorage.setItem(
    `${DRAFT_DECK_SESSION_KEY}:${gameId}`,
    JSON.stringify(data),
  );
}

function pickBotSeat(usedSeats: number[]): number {
  const available = [1, 2, 3, 4, 5, 6, 7].filter((s) => !usedSeats.includes(s));
  if (available.length === 0) return Math.floor(Math.random() * 7) + 1;
  return available[Math.floor(Math.random() * available.length)];
}

// ── Persistence helpers ─────────────────────────────────────────────────

let debounceTimer: ReturnType<typeof setTimeout> | null = null;

function persistDraft(): void {
  const { adapter, draftId, phase, view, mainDeck, landCounts, poolSortMode, poolPanelOpen, difficulty, selectedSet } =
    useDraftStore.getState();
  if (!adapter || !draftId || !selectedSet || phase === "setup" || phase === "launching" || phase === "playing" || phase === "complete") return;
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
        pickCount: view?.pool.length ?? 0,
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
      const oldMeta = loadActiveQuickDraft();
      if (oldMeta) {
        void clearDraftRun(oldMeta.id);
      }

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
        runState: null,
      });

      persistDraft();
    },

    resumeDraft: async () => {
      const meta = loadActiveQuickDraft();
      if (!meta) return;

      const run = await loadDraftRun(meta.id);

      if (meta.phase === "playing" || meta.phase === "complete") {
        if (!run) {
          await clearQuickDraftSession(meta.id);
          clearActiveQuickDraft();
          return;
        }

        const saved = await loadQuickDraftSession(meta.id);
        if (!saved) {
          await clearQuickDraftSession(meta.id);
          await clearDraftRun(meta.id);
          clearActiveQuickDraft();
          return;
        }

        const adapter = new DraftAdapter();

        if (meta.difficulty >= 3) {
          const resp = await fetch(__CARD_DATA_URL__);
          const json = await resp.text();
          await adapter.loadCardDatabase(json);
        }

        const view = await adapter.importSession(saved.sessionJson, meta.difficulty);

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
          runFormat: run.format,
          runState: run,
        });
        return;
      }

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

        const view = await adapter.importSession(saved.sessionJson, meta.difficulty);

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
          runFormat: run?.format ?? "run",
          runState: run,
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
        await clearDraftRun(id);
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
      const { adapter, mainDeck, landCounts } = get();
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

    setRunFormat: (f) => {
      set({ runFormat: f });
    },

    launchMatch: async (navigate) => {
      const { adapter, mainDeck, landCounts, difficulty, draftId, selectedSet, runFormat } = get();
      if (!adapter || !draftId || !selectedSet) return;

      const fullDeck = [...mainDeck, ...expandLands(landCounts)];
      const botSeat = pickBotSeat([]);
      const botDeck = await adapter.getBotDeck(botSeat);
      const botFullDeck = [
        ...botDeck.main_deck,
        ...Object.entries(botDeck.lands).flatMap(([name, count]) =>
          Array<string>(count).fill(name),
        ),
      ];

      const gameId = crypto.randomUUID();
      storeDraftDeckData(gameId, fullDeck, botFullDeck);

      const matchType = runFormat === "bo3" ? "bo3" : "bo1";

      const newRunState: DraftRunState = {
        format: runFormat,
        results: [],
        playerDeck: fullDeck,
        opponentDeck: botFullDeck,
        usedBotSeats: [botSeat],
      };

      set({ phase: "playing", runState: newRunState });

      await saveDraftRun(draftId, newRunState);
      saveActiveQuickDraft({
        id: draftId,
        setCode: selectedSet,
        difficulty,
        phase: "playing",
        pickCount: get().view?.pool.length ?? 0,
        updatedAt: Date.now(),
        runFormat,
        runWins: 0,
        runLosses: 0,
        runDraws: 0,
        currentGameId: gameId,
      });

      const headDifficulty = DIFFICULTY_NAMES[difficulty] ?? "Medium";
      useGameStore.setState({ gameId });
      navigate(
        `/game/${gameId}?mode=ai&difficulty=${headDifficulty}&format=Limited&match=${matchType}&source=draft&draftId=${draftId}`,
      );
    },

    recordMatchResult: async (gameId, result) => {
      const meta = loadActiveQuickDraft();
      if (!meta || !meta.runFormat) return;

      const run = await loadDraftRun(meta.id);
      if (!run) return;

      if (run.results.some((r) => r.gameId === gameId)) return;

      const updatedResults = [...run.results, { gameId, result }];
      const wins = updatedResults.filter((r) => r.result === "win").length;
      const losses = updatedResults.filter((r) => r.result === "loss").length;
      const draws = updatedResults.filter((r) => r.result === "draw").length;

      const limits = runLimits(meta.runFormat);
      const isComplete = wins >= limits.maxWins || losses >= limits.maxLosses;

      const updatedRunState: DraftRunState = { ...run, results: updatedResults };
      const updatedPhase: DraftPhase = isComplete ? "complete" : "playing";

      set({ runState: updatedRunState, phase: updatedPhase });

      await saveDraftRun(meta.id, updatedRunState);
      saveActiveQuickDraft({
        ...meta,
        phase: updatedPhase,
        updatedAt: Date.now(),
        runWins: wins,
        runLosses: losses,
        runDraws: draws,
        currentGameId: undefined,
      });
    },

    launchNextMatch: async (navigate) => {
      const { adapter, draftId, selectedSet, difficulty, runState, runFormat } = get();
      if (!adapter || !draftId || !selectedSet || !runState) return;

      const botSeat = pickBotSeat(runState.usedBotSeats);
      const botDeck = await adapter.getBotDeck(botSeat);
      const botFullDeck = [
        ...botDeck.main_deck,
        ...Object.entries(botDeck.lands).flatMap(([name, count]) =>
          Array<string>(count).fill(name),
        ),
      ];

      const gameId = crypto.randomUUID();
      storeDraftDeckData(gameId, runState.playerDeck, botFullDeck);

      const matchType = runFormat === "bo3" ? "bo3" : "bo1";

      const usedBotSeats = runState.usedBotSeats.length >= 7
        ? [botSeat]
        : [...runState.usedBotSeats, botSeat];

      const updatedRunState: DraftRunState = { ...runState, usedBotSeats, opponentDeck: botFullDeck };
      const wins = runState.results.filter((r) => r.result === "win").length;
      const losses = runState.results.filter((r) => r.result === "loss").length;
      const draws = runState.results.filter((r) => r.result === "draw").length;

      set({ runState: updatedRunState });

      await saveDraftRun(draftId, updatedRunState);
      saveActiveQuickDraft({
        id: draftId,
        setCode: selectedSet,
        difficulty,
        phase: "playing",
        pickCount: get().view?.pool.length ?? 0,
        updatedAt: Date.now(),
        runFormat,
        runWins: wins,
        runLosses: losses,
        runDraws: draws,
        currentGameId: gameId,
      });

      const headDifficulty = DIFFICULTY_NAMES[difficulty] ?? "Medium";
      useGameStore.setState({ gameId });
      navigate(
        `/game/${gameId}?mode=ai&difficulty=${headDifficulty}&format=Limited&match=${matchType}&source=draft&draftId=${draftId}`,
      );
    },

    endRun: async () => {
      const { draftId } = get();
      if (!draftId) return;

      await clearQuickDraftSession(draftId);
      await clearDraftRun(draftId);
      clearActiveQuickDraft();
      set(initialState);
    },

    reset: () => {
      if (debounceTimer) clearTimeout(debounceTimer);
      set(initialState);
    },
  }),
);
