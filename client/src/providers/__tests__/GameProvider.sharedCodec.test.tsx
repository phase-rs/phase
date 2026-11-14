import { cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { getSharedAdapter } from "../../adapter/wasm-adapter";
import { useGameStore } from "../../stores/gameStore";
import { usePreferencesStore } from "../../stores/preferencesStore";
import { buildGameState } from "../../test/factories/gameStateFactory";
import type { FormatConfig } from "../../adapter/types";
import type * as GamePersistence from "../../services/gamePersistence";
import type * as FormatRegistry from "../../data/formatRegistry";
import type * as PileSource from "../../services/pileSource";
import { GameProvider } from "../GameProvider";

const FORMAT: FormatConfig = {
  format: "Momir",
  starting_life: 20,
  min_players: 2,
  max_players: 2,
  deck_size: { type: "Exactly", data: 60 },
  singleton: false,
  command_zone: false,
  commander_damage_threshold: null,
  poison_counter_threshold: 10,
  range_of_influence: null,
  team_based: false,
  sideboard_policy: { type: "Forbidden" },
  default_deck_copy_limit: { type: "UpTo", data: 4 },
  uses_commander: false,
  allow_debug_actions: false,
};

const io = vi.hoisted(() => {
  const sessions = new Set<number>();
  return {
    sessions,
    initialize: vi.fn(async () => undefined),
    initializeGame: vi.fn(async () => ({ events: [], log_entries: [] })),
    getSnapshot: vi.fn(),
    loadCardDbFromUrl: vi.fn(async () => 100),
    resetGame: vi.fn(async () => undefined),
    openStateCodec: vi.fn(async () => { sessions.add(1); return 1; }),
    decodeStateCodec: vi.fn(async (id: number, _frame: Uint8Array) => {
      if (!sessions.has(id)) throw new Error("Codec session closed");
      return { kind: "decoded", json: '{"revision":2}' };
    }),
    closeStateCodec: vi.fn(async (id: number) => { sessions.delete(id); }),
    dispose: vi.fn(),
    ensureNativeEngine: vi.fn(async () => { throw new Error("Native startup failed"); }),
  };
});

vi.mock("../../adapter/engine-worker-client", () => ({
  EngineWorkerClient: vi.fn().mockImplementation(function () { return io; }),
}));
vi.mock("../../services/nativeEngine", () => ({
  canAttemptNativeEngine: (enabled: boolean) => enabled,
  nativeEngineKeyForCurrentOrigin: () => ({ release: { version: "test" } }),
  ensureNativeEngine: io.ensureNativeEngine,
}));
vi.mock("../../services/gamePersistence", async (original) => ({
  ...await original<typeof GamePersistence>(),
  loadGame: vi.fn(async () => null),
  loadActiveGame: vi.fn(() => null),
  saveActiveGame: vi.fn(),
  saveAuthoritativeGame: vi.fn(async () => undefined),
}));
vi.mock("../../data/formatRegistry", async (original) => ({
  ...await original<typeof FormatRegistry>(),
  formatSuppliesDeck: () => true,
}));
vi.mock("../../services/pileSource", async (original) => ({
  ...await original<typeof PileSource>(),
  pileSeatDeck: vi.fn(async () => ({ main_deck: [], sideboard: [], commander: [] })),
}));
vi.mock("../../game/controllers/gameLoopController", () => ({
  createGameLoopController: () => ({ start: vi.fn(), dispose: vi.fn() }),
}));
vi.mock("../../hooks/useGameplayPreferencesSync", () => ({ useGameplayPreferencesSync: vi.fn() }));
vi.mock("../../audio/AudioManager", () => ({ audioManager: { setContext: vi.fn() } }));

// Exercise the actual provider, Zustand reset action, and WasmAdapter lease owner.
// Only the engine worker transport and unrelated browser/native I/O are replaced.
describe("GameProvider shared codec ownership", () => {
  afterEach(() => {
    cleanup();
    useGameStore.getState().reset();
  });

  it("clears a failed-native WASM fallback without closing another server codec lease", async () => {
    usePreferencesStore.setState({ nativeEngineEnabled: true });
    io.getSnapshot.mockResolvedValue({
      state: buildGameState(),
      legalResult: { actions: [], autoPassRecommended: false },
    });
    const shared = getSharedAdapter();
    // The lease belongs to an independent server consumer of this same facade.
    const serverCodec = await shared.openStateCodecSession();
    expect(serverCodec).not.toBeNull();
    if (!serverCodec) throw new Error("Server codec lease was not opened");
    const frame = new Uint8Array([1, 2, 3]);
    await expect(serverCodec.decode(frame)).resolves.toEqual({ kind: "decoded", json: '{"revision":2}' });

    const view = render(
      <GameProvider gameId="native-fallback-shared-codec" mode="ai" formatConfig={FORMAT}>
        <div />
      </GameProvider>,
    );
    await waitFor(() => {
      expect(io.ensureNativeEngine).toHaveBeenCalledOnce();
      expect(useGameStore.getState().gameId).toBe("native-fallback-shared-codec");
      expect(useGameStore.getState().adapter).toBe(shared);
      expect(useGameStore.getState().engineMode).toBe("wasm");
    });
    expect(useGameStore.getState().gameState).not.toBeNull();

    view.unmount();
    await waitFor(() => expect(useGameStore.getState().adapter).toBeNull());
    expect(useGameStore.getState().gameId).toBeNull();
    expect(useGameStore.getState().gameState).toBeNull();
    await expect(serverCodec.decode(frame)).resolves.toEqual({ kind: "decoded", json: '{"revision":2}' });
    expect(io.resetGame).toHaveBeenCalledOnce();
    expect(io.closeStateCodec).not.toHaveBeenCalled();

    shared.dispose();
    await waitFor(() => expect(io.closeStateCodec).toHaveBeenCalledExactlyOnceWith(1));
    await expect(serverCodec.decode(frame)).rejects.toThrow();
    serverCodec.close();
    expect(io.closeStateCodec).toHaveBeenCalledOnce();
  });
});
