import { beforeEach, describe, expect, it, vi } from "vitest";

import type { EngineAdapter, GameState, TrustedGameStateEnvelope } from "../../adapter/types";
import { persistedGameStateView } from "../../adapter/types";
import { GAME_KEY_PREFIX } from "../../constants/storage";
import { buildGameState, buildPriorityWaitingFor } from "../../test/factories/gameStateFactory";

vi.mock("idb-keyval", () => ({
  createStore: vi.fn(() => ({})),
  del: vi.fn().mockResolvedValue(undefined),
  get: vi.fn().mockResolvedValue(undefined),
  set: vi.fn().mockResolvedValue(undefined),
}));

import { get as idbGet, set as idbSet } from "idb-keyval";
import { loadGame, saveAuthoritativeGame } from "../gamePersistence";

function fixtureState(): GameState {
  return buildGameState({
    players: [],
    rng_seed: 42,
    waiting_for: buildPriorityWaitingFor(),
  });
}

describe("game persistence", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("retains the engine-authored trusted envelope in IndexedDB", async () => {
    const state = fixtureState();
    const envelope: TrustedGameStateEnvelope = {
      state,
      precast_shortcut_runtime: { opaque: true },
    };

    const adapter = {
      exportPersistenceState: vi.fn().mockResolvedValue(JSON.stringify(envelope)),
    } as unknown as EngineAdapter;
    await saveAuthoritativeGame("trusted-local", adapter, state);

    expect(adapter.exportPersistenceState).toHaveBeenCalledOnce();
    expect(idbSet).toHaveBeenCalledWith(
      GAME_KEY_PREFIX + "trusted-local",
      envelope,
      expect.anything(),
    );
    vi.mocked(idbGet).mockResolvedValueOnce(envelope);
    const restored = await loadGame("trusted-local");
    expect(restored).toEqual(envelope);
    expect(persistedGameStateView(restored!)).toEqual(state);
  });
});
