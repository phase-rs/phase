import { create } from "zustand";

import type { GameState, ReplayHeader } from "../adapter/types";
import { ReplayAdapter } from "../adapter/replay-adapter";
import { useGameStore } from "./gameStore";

interface ReplayStoreState {
  adapter: ReplayAdapter | null;
  header: ReplayHeader | null;
  /** Total recorded actions. Valid scrub positions are `0..=totalActions`. */
  totalActions: number;
  currentIndex: number;
  isPlaying: boolean;
  /** Actions advanced per second while playing. */
  playbackSpeed: number;
  isLoading: boolean;
  error: string | null;
}

interface ReplayStoreActions {
  /** Parse and load a replay log (the JSON `replayExport.ts`/`exportReplayLog` produced). */
  loadReplay: (replayJson: string) => Promise<void>;
  seek: (index: number) => Promise<void>;
  stepForward: () => Promise<void>;
  stepBackward: () => Promise<void>;
  play: () => void;
  pause: () => void;
  setSpeed: (actionsPerSecond: number) => void;
  /** Tear down the loaded replay and clear the shared game store. */
  unload: () => void;
}

export type ReplayStore = ReplayStoreState & ReplayStoreActions;

let playTimer: ReturnType<typeof setInterval> | null = null;

function stopPlayTimer(): void {
  if (playTimer !== null) {
    clearInterval(playTimer);
    playTimer = null;
  }
}

/**
 * Mirror the reconstructed state into the shared `gameStore` so the existing
 * board UI (`GameBoard` and friends, which read from `useGameStore` directly
 * rather than via props) renders it. `gameMode: "spectate"` — set once in
 * `loadReplay` — already disables all action dispatch (see
 * `game/dispatch.ts`, `hooks/usePlayerId.ts`), so this is read-only from the
 * board's perspective without needing any GamePage/GameProvider changes.
 */
function pushStateToGameStore(state: GameState | null): void {
  useGameStore.setState({
    gameState: state,
    waitingFor: state?.waiting_for ?? null,
  });
}

const initialState: ReplayStoreState = {
  adapter: null,
  header: null,
  totalActions: 0,
  currentIndex: 0,
  isPlaying: false,
  playbackSpeed: 2,
  isLoading: false,
  error: null,
};

export const useReplayStore = create<ReplayStore>()((set, get) => ({
  ...initialState,

  loadReplay: async (replayJson) => {
    get().unload();
    set({ isLoading: true, error: null });

    const adapter = new ReplayAdapter();
    try {
      await adapter.initialize();
      const totalActions = await adapter.loadReplay(replayJson);
      const header = await adapter.header();
      const state = await adapter.seek(0);

      useGameStore.setState({
        gameId: "replay",
        gameMode: "spectate",
        adapter,
        legalActions: [],
        legalActionsByObject: {},
        autoPassRecommended: false,
        spellCosts: {},
        stateHistory: [],
        turnCheckpoints: [],
      });
      pushStateToGameStore(state);

      set({ adapter, header, totalActions, currentIndex: 0, isLoading: false });
    } catch (err) {
      adapter.dispose();
      set({ isLoading: false, error: err instanceof Error ? err.message : String(err) });
      throw err;
    }
  },

  seek: async (index) => {
    const { adapter, totalActions } = get();
    if (!adapter) return;
    const clamped = Math.max(0, Math.min(index, totalActions));
    try {
      const state = await adapter.seek(clamped);
      set({ currentIndex: clamped, error: null });
      pushStateToGameStore(state);
    } catch (err) {
      // A reconstruction desync (ReplayError::Desync — see
      // crates/engine/src/game/replay.rs) is a real failure, not "nothing to
      // show": surface it instead of silently leaving the board on the last
      // good frame. Caught here (rather than left to propagate) because
      // `seek` is driven from fire-and-forget call sites (the scrubber
      // input, the play-loop interval) that can't otherwise react to a
      // rejected promise without producing an unhandled rejection.
      get().pause();
      set({ error: err instanceof Error ? err.message : String(err) });
    }
  },

  stepForward: async () => {
    await get().seek(get().currentIndex + 1);
  },

  stepBackward: async () => {
    await get().seek(get().currentIndex - 1);
  },

  play: () => {
    stopPlayTimer();
    set({ isPlaying: true });
    playTimer = setInterval(() => {
      const { currentIndex, totalActions } = get();
      if (currentIndex >= totalActions) {
        get().pause();
        return;
      }
      void get().seek(currentIndex + 1);
    }, 1000 / Math.max(get().playbackSpeed, 0.1));
  },

  pause: () => {
    stopPlayTimer();
    set({ isPlaying: false });
  },

  setSpeed: (actionsPerSecond) => {
    set({ playbackSpeed: actionsPerSecond });
    if (get().isPlaying) {
      get().play();
    }
  },

  unload: () => {
    stopPlayTimer();
    get().adapter?.dispose();
    set({ ...initialState });
    useGameStore.setState({
      gameId: null,
      gameMode: null,
      gameState: null,
      adapter: null,
      waitingFor: null,
    });
  },
}));
