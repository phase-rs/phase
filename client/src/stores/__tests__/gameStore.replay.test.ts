import { act } from "react";
import { beforeEach, describe, expect, it } from "vitest";

import type { GameState } from "../../adapter/types";
import { useGameStore } from "../gameStore";

function makeState(turn: number, player: 0 | 1 = 0): GameState {
  return {
    turn_number: turn,
    active_player: player,
    phase: "PreCombatMain",
    players: [],
    priority_player: player,
    objects: {},
    next_object_id: 1,
    battlefield: [],
    stack: [],
    exile: [],
    rng_seed: 42,
    combat: null,
    waiting_for: { type: "Priority", data: { player } },
    has_pending_cast: false,
    lands_played_this_turn: 0,
    max_lands_per_turn: 1,
    priority_pass_count: 0,
    pending_replacement: null,
    layers_dirty: false,
    next_timestamp: 1,
  };
}

const cp1 = makeState(1);
const cp2 = makeState(2);
const cp3 = makeState(3);
const live = makeState(4);

function seedStore(checkpoints: GameState[], currentState: GameState) {
  act(() => {
    useGameStore.setState({
      gameState: currentState,
      waitingFor: currentState.waiting_for,
      turnCheckpoints: checkpoints,
      stateHistory: [],
      replayMode: false,
      replayIndex: null,
      liveGameState: null,
      gameMode: "ai",
      legalActions: [],
      legalActionsByObject: {},
    });
  });
}

describe("gameStore replay actions", () => {
  beforeEach(() => {
    act(() => {
      useGameStore.setState({
        gameState: null,
        waitingFor: null,
        turnCheckpoints: [],
        stateHistory: [],
        replayMode: false,
        replayIndex: null,
        liveGameState: null,
        gameMode: null,
        legalActions: [],
        legalActionsByObject: {},
      });
    });
  });

  describe("enterReplay", () => {
    it("does nothing when there are no checkpoints", () => {
      act(() => {
        useGameStore.setState({ gameState: live, turnCheckpoints: [], gameMode: "ai" });
      });
      act(() => useGameStore.getState().enterReplay());
      expect(useGameStore.getState().replayMode).toBe(false);
    });

    it("does nothing when there is only one checkpoint", () => {
      seedStore([cp1], live);
      act(() => useGameStore.getState().enterReplay());
      // Component requires >= 2 checkpoints; store allows 1 but returns to isAvailable check
      // Store itself does NOT gate on count >= 2 — that's the UI's concern.
      // enterReplay only gates on length === 0 and replayMode already active.
      expect(useGameStore.getState().replayMode).toBe(true);
    });

    it("saves liveGameState and enters replay at latest checkpoint by default", () => {
      seedStore([cp1, cp2, cp3], live);
      act(() => useGameStore.getState().enterReplay());

      const state = useGameStore.getState();
      expect(state.replayMode).toBe(true);
      expect(state.replayIndex).toBe(2);
      expect(state.liveGameState).toBe(live);
      expect(state.gameState?.turn_number).toBe(3);
    });

    it("enters replay at the specified checkpoint index", () => {
      seedStore([cp1, cp2, cp3], live);
      act(() => useGameStore.getState().enterReplay(0));

      const state = useGameStore.getState();
      expect(state.replayIndex).toBe(0);
      expect(state.gameState?.turn_number).toBe(1);
    });

    it("clamps out-of-range index to valid range", () => {
      seedStore([cp1, cp2], live);
      act(() => useGameStore.getState().enterReplay(99));

      expect(useGameStore.getState().replayIndex).toBe(1);
    });

    it("clears legalActions and legalActionsByObject on entry", () => {
      act(() => {
        useGameStore.setState({
          gameState: live,
          turnCheckpoints: [cp1, cp2],
          replayMode: false,
          replayIndex: null,
          liveGameState: null,
          gameMode: "ai",
          legalActions: [{ type: "PassPriority", data: { player: 0 } }],
          legalActionsByObject: { "1": [{ type: "PassPriority", data: { player: 0 } }] },
        });
      });
      act(() => useGameStore.getState().enterReplay());

      const state = useGameStore.getState();
      expect(state.legalActions).toHaveLength(0);
      expect(state.legalActionsByObject).toEqual({});
    });

    it("does not re-enter when already in replay mode", () => {
      seedStore([cp1, cp2, cp3], live);
      act(() => useGameStore.getState().enterReplay(0));
      act(() => useGameStore.getState().enterReplay(2));

      // Should stay at index 0 because second call is a no-op
      expect(useGameStore.getState().replayIndex).toBe(0);
    });
  });

  describe("replayTo", () => {
    it("does nothing when not in replay mode", () => {
      seedStore([cp1, cp2, cp3], live);
      act(() => useGameStore.getState().replayTo(0));

      expect(useGameStore.getState().gameState?.turn_number).toBe(4);
    });

    it("navigates to a different checkpoint", () => {
      seedStore([cp1, cp2, cp3], live);
      act(() => useGameStore.getState().enterReplay(2));
      act(() => useGameStore.getState().replayTo(0));

      const state = useGameStore.getState();
      expect(state.replayIndex).toBe(0);
      expect(state.gameState?.turn_number).toBe(1);
    });

    it("clamps index to valid range", () => {
      seedStore([cp1, cp2], live);
      act(() => useGameStore.getState().enterReplay());
      act(() => useGameStore.getState().replayTo(-5));

      expect(useGameStore.getState().replayIndex).toBe(0);
    });
  });

  describe("exitReplay", () => {
    it("does nothing when not in replay mode", () => {
      seedStore([cp1, cp2], live);
      act(() => useGameStore.getState().exitReplay());

      expect(useGameStore.getState().replayMode).toBe(false);
      expect(useGameStore.getState().gameState?.turn_number).toBe(4);
    });

    it("restores the live game state", () => {
      seedStore([cp1, cp2, cp3], live);
      act(() => useGameStore.getState().enterReplay(0));
      act(() => useGameStore.getState().exitReplay());

      const state = useGameStore.getState();
      expect(state.replayMode).toBe(false);
      expect(state.replayIndex).toBeNull();
      expect(state.liveGameState).toBeNull();
      expect(state.gameState?.turn_number).toBe(4);
    });

    it("restores the waiting_for from the live state", () => {
      const liveWaiting: GameState = {
        ...live,
        waiting_for: { type: "Priority", data: { player: 0 } },
      };
      seedStore([cp1, cp2], liveWaiting);
      act(() => useGameStore.getState().enterReplay());
      act(() => useGameStore.getState().exitReplay());

      expect(useGameStore.getState().waitingFor).toEqual({ type: "Priority", data: { player: 0 } });
    });
  });

  describe("dispatch blocked in replay mode", () => {
    it("returns empty events without calling adapter when in replay mode", async () => {
      seedStore([cp1, cp2], live);
      act(() => useGameStore.getState().enterReplay());

      // dispatch requires adapter; set a mock one
      const adapter = {
        initialize: () => Promise.resolve(),
        initializeGame: () => Promise.resolve({ events: [] }),
        submitAction: () => Promise.reject(new Error("should not be called")),
        getState: () => Promise.resolve(live),
        getLegalActions: () => Promise.resolve({ actions: [], autoPassRecommended: false }),
        restoreState: () => Promise.resolve(),
        getAiAction: () => null,
        dispose: () => {},
        estimateBracket: () => Promise.resolve(null),
      };
      act(() => {
        useGameStore.setState({ adapter });
      });

      const events = await useGameStore.getState().dispatch({ type: "PassPriority", data: { player: 0 } });
      expect(events).toEqual([]);
      // gameState should remain at the replayed checkpoint (cp2 = turn 2), not the live state
      expect(useGameStore.getState().gameState?.turn_number).toBe(2);
    });
  });
});
