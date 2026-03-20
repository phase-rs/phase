import { describe, it, expect, vi, beforeEach } from "vitest";
import { WasmAdapter } from "../wasm-adapter";
import type { EngineAdapter } from "../types";
import { AdapterError, AdapterErrorCode } from "../types";

// Mock the WASM module
vi.mock("@wasm/engine", () => {
  const mockInit = vi.fn().mockResolvedValue(undefined);
  const mockPing = vi.fn().mockReturnValue("phase-rs engine ready");
  const mockCreateInitialState = vi.fn().mockReturnValue({
    turn_number: 1,
    active_player: 0,
    phase: "Untap",
    players: [
      { id: 0, life: 20, mana_pool: { mana: [] } },
      { id: 1, life: 20, mana_pool: { mana: [] } },
    ],
    priority_player: 0,
  });
  const mockInitializeGame = vi.fn().mockReturnValue({
    events: [{ type: "GameStarted" }],
    waiting_for: { type: "Priority", data: { player: 0 } },
  });
  const mockSubmitAction = vi.fn().mockReturnValue({
    events: [],
    waiting_for: { type: "Priority", data: { player: 0 } },
  });
  const mockGetGameState = vi.fn().mockReturnValue(null);

  const mockRestoreGameState = vi.fn();

  return {
    default: mockInit,
    ping: mockPing,
    create_initial_state: mockCreateInitialState,
    initialize_game: mockInitializeGame,
    submit_action: mockSubmitAction,
    get_game_state: mockGetGameState,
    restore_game_state: mockRestoreGameState,
  };
});

describe("WasmAdapter", () => {
  let adapter: WasmAdapter;

  beforeEach(() => {
    vi.clearAllMocks();
    adapter = new WasmAdapter();
  });

  it("implements EngineAdapter interface", () => {
    const _check: EngineAdapter = adapter;
    expect(_check).toBeDefined();
    expect(typeof adapter.initialize).toBe("function");
    expect(typeof adapter.submitAction).toBe("function");
    expect(typeof adapter.getState).toBe("function");
    expect(typeof adapter.dispose).toBe("function");
  });

  describe("initialize", () => {
    it("calls WASM init and sets initialized flag", async () => {
      await adapter.initialize();
      // Calling getState should work after initialization
      const state = await adapter.getState();
      expect(state).toBeDefined();
      expect(state.turn_number).toBe(1);
    });

    it("is idempotent - second call is a no-op", async () => {
      await adapter.initialize();
      await adapter.initialize();
      const { default: init } = await import("@wasm/engine");
      expect(init).toHaveBeenCalledTimes(1);
    });
  });

  describe("submitAction", () => {
    it("throws AdapterError with NOT_INITIALIZED if not initialized", async () => {
      await expect(
        adapter.submitAction({ type: "PassPriority" }),
      ).rejects.toThrow(AdapterError);

      try {
        await adapter.submitAction({ type: "PassPriority" });
      } catch (error) {
        expect(error).toBeInstanceOf(AdapterError);
        const adapterError = error as AdapterError;
        expect(adapterError.code).toBe(AdapterErrorCode.NOT_INITIALIZED);
        expect(adapterError.recoverable).toBe(true);
      }
    });

    it("processes actions sequentially via queue", async () => {
      await adapter.initialize();

      const order: number[] = [];

      // Submit multiple actions concurrently
      const p1 = adapter.submitAction({ type: "PassPriority" }).then(() => order.push(1));
      const p2 = adapter.submitAction({ type: "PassPriority" }).then(() => order.push(2));
      const p3 = adapter.submitAction({ type: "PassPriority" }).then(() => order.push(3));

      await Promise.all([p1, p2, p3]);

      // Actions should resolve in order (serialized queue)
      expect(order).toEqual([1, 2, 3]);
    });
  });

  describe("getState", () => {
    it("throws if not initialized", async () => {
      await expect(adapter.getState()).rejects.toThrow(AdapterError);
    });

    it("returns game state after initialization", async () => {
      await adapter.initialize();
      const state = await adapter.getState();
      expect(state.turn_number).toBe(1);
      expect(state.active_player).toBe(0);
      expect(state.phase).toBe("Untap");
      expect(state.players).toHaveLength(2);
    });
  });

  describe("dispose", () => {
    it("cleans up state and prevents further operations", async () => {
      await adapter.initialize();
      adapter.dispose();

      // Should throw NOT_INITIALIZED after dispose
      await expect(adapter.getState()).rejects.toThrow(AdapterError);
    });
  });

  describe("restoreState", () => {
    it("calls restore_game_state with the provided state", async () => {
      await adapter.initialize();

      const mockState = {
        turn_number: 3,
        active_player: 0,
        phase: "PreCombatMain",
        players: [],
        priority_player: 0,
      } as unknown as import("../../adapter/types").GameState;

      const { restore_game_state } = await import("@wasm/engine");
      adapter.restoreState(mockState);
      expect(restore_game_state).toHaveBeenCalledWith(JSON.stringify(mockState));
    });

    it("throws if not initialized", () => {
      const mockState = {} as import("../../adapter/types").GameState;
      expect(() => adapter.restoreState(mockState)).toThrow(AdapterError);
    });
  });

  describe("initialize_game", () => {
    it("returns events from initialize_game binding", async () => {
      await adapter.initialize();
      const result = adapter.initializeGame();
      expect(result.events).toEqual([{ type: "GameStarted" }]);
    });
  });

  describe("getState returns current game state", () => {
    it("returns state from get_game_state when available", async () => {
      const fullState = {
        turn_number: 5,
        active_player: 1,
        phase: "Draw",
        players: [
          { id: 0, life: 18, mana_pool: { mana: [] } },
          { id: 1, life: 20, mana_pool: { mana: [] } },
        ],
        priority_player: 1,
      };

      const { get_game_state } = await import("@wasm/engine");
      vi.mocked(get_game_state).mockReturnValue(fullState);

      await adapter.initialize();
      const state = await adapter.getState();
      expect(state.turn_number).toBe(5);
      expect(state.active_player).toBe(1);
      expect(state.phase).toBe("Draw");
    });
  });

  describe("error normalization", () => {
    it("wraps WASM errors into AdapterError with recoverable flag", async () => {
      const { create_initial_state, get_game_state } = await import("@wasm/engine");
      vi.mocked(get_game_state).mockReturnValue(null);
      vi.mocked(create_initial_state).mockImplementation(() => {
        throw new Error("WASM execution failed");
      });

      await adapter.initialize();

      try {
        await adapter.getState();
        expect.fail("should have thrown");
      } catch (error) {
        expect(error).toBeInstanceOf(AdapterError);
        const adapterError = error as AdapterError;
        expect(adapterError.code).toBe(AdapterErrorCode.WASM_ERROR);
        expect(adapterError.message).toContain("WASM execution failed");
        expect(adapterError.recoverable).toBe(false);
      }
    });
  });
});
