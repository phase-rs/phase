import { beforeEach, describe, expect, it, vi } from "vitest";

import type {
  CustomFormatRules,
  EngineAdapter,
  FormatConfig,
  GameState,
  TrustedGameStateEnvelope,
} from "../../adapter/types";
import { persistedGameStateView } from "../../adapter/types";
import { ACTIVE_GAME_KEY, GAME_CHECKPOINTS_PREFIX, GAME_KEY_PREFIX } from "../../constants/storage";
import { buildGameState, buildPriorityWaitingFor } from "../../test/factories/gameStateFactory";

vi.mock("idb-keyval", () => ({
  createStore: vi.fn(() => ({})),
  del: vi.fn().mockResolvedValue(undefined),
  get: vi.fn().mockResolvedValue(undefined),
  set: vi.fn().mockResolvedValue(undefined),
}));

import { del as idbDel, get as idbGet, set as idbSet } from "idb-keyval";
import {
  clearGame,
  clearGameStrict,
  loadGame,
  loadGameStrict,
  loadCheckpoints,
  loadP2PHostSession,
  migratePersistedGameState,
  saveAuthoritativeGame,
  saveAuthoritativeGameStrict,
  saveGame,
  saveResumableGameStrict,
  saveActiveGame,
} from "../gamePersistence";

function fixtureState(): GameState {
  return buildGameState({
    players: [],
    rng_seed: 42,
    waiting_for: buildPriorityWaitingFor(),
  });
}

function customCommandZoneMinimumRules(): CustomFormatRules {
  return {
    id: 7,
    structural: {
      starting_life: 20,
      min_players: 2,
      max_players: 4,
      deck_size: { type: "Minimum", data: 60 },
      singleton: false,
      command_zone_mode: {
        Enabled: {
          commander_damage_threshold: null,
          eligibility_rule: "FreeformAnyCastableCard",
        },
      },
      range_of_influence: null,
      team_based: false,
      sideboard_policy: { type: "Unlimited" },
      default_deck_copy_limit: { type: "Unlimited" },
    },
    legality: {
      legal_sets: null,
      banned: [],
      restricted: [],
      legacy: {
        mana_burn: "Modern",
        damage_timing: "Modern",
        wish_scope: "PostM10SideboardOnly",
        legend_rule_scope: "Modern",
      },
    },
  };
}

describe("game persistence", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    localStorage.clear();
    vi.mocked(idbDel).mockResolvedValue(undefined);
    vi.mocked(idbGet).mockReset();
    vi.mocked(idbGet).mockResolvedValue(undefined);
    vi.mocked(idbSet).mockReset();
    vi.mocked(idbSet).mockResolvedValue(undefined);
  });

  it("deletes the three exact game records in order before clearing matching active metadata", async () => {
    saveActiveGame({ id: "target", mode: "ai", difficulty: "Medium" });
    const keys = [GAME_CHECKPOINTS_PREFIX + "target", "phase-p2p-host:target", GAME_KEY_PREFIX + "target"];
    const records = new Map<string, unknown>(keys.map((key) => [key, fixtureState()]));
    vi.mocked(idbGet).mockImplementation(async (key) => records.get(String(key)));
    const releases: Array<() => void> = [];
    let settled = false;
    vi.mocked(idbDel).mockImplementation((key) => new Promise((resolve) => {
      releases.push(() => { records.delete(String(key)); resolve(); });
    }));
    const pending = clearGameStrict("target");
    void pending.then(() => { settled = true; });
    for (let index = 0; index < keys.length; index += 1) {
      expect(idbDel).toHaveBeenCalledTimes(index + 1);
      expect(idbDel).toHaveBeenNthCalledWith(index + 1, keys[index], expect.anything());
      expect(localStorage.getItem(ACTIVE_GAME_KEY)).not.toBeNull();
      expect(records.has(keys[2])).toBe(true);
      expect(settled).toBe(false);
      releases[index]();
      await Promise.resolve();
    }
    await pending;
    expect(localStorage.getItem(ACTIVE_GAME_KEY)).toBeNull();
    expect(keys.every((key) => !records.has(key))).toBe(true);
    const calls = vi.mocked(idbDel).mock.calls;
    expect(calls[0][1]).toBe(calls[1][1]);
    expect(calls[1][1]).toBe(calls[2][1]);
    expect(calls.map(([key]) => key)).toEqual(keys);
  });

  it.each([0, 1, 2])("retains the game sentinel after delete index %i rejects, then retries safely", async (failedIndex) => {
    saveActiveGame({ id: "target", mode: "ai", difficulty: "Medium" });
    const keys = [GAME_CHECKPOINTS_PREFIX + "target", "phase-p2p-host:target", GAME_KEY_PREFIX + "target"];
    const otherKeys = keys.map((key) => key.replace("target", "other"));
    const snapshot = fixtureState();
    const records = new Map<string, unknown>([
      [keys[0], [snapshot]], [keys[1], { roomCode: "ABCDE" }], [keys[2], snapshot],
      [otherKeys[0], [snapshot]], [otherKeys[1], { roomCode: "OTHER" }], [otherKeys[2], snapshot],
    ]);
    vi.mocked(idbGet).mockImplementation(async (key) => records.get(String(key)));
    const original = new Error(`delete ${failedIndex} failed`);
    let rejectAt: number | null = failedIndex;
    vi.mocked(idbDel).mockImplementation(async (key) => {
      const index = vi.mocked(idbDel).mock.calls.length - 1;
      if (index === rejectAt) throw original; // rejected IDB transaction did not commit
      records.delete(String(key));
    });
    await expect(clearGameStrict("target")).rejects.toBe(original);
    expect(vi.mocked(idbDel).mock.calls.map(([key]) => key)).toEqual(keys.slice(0, failedIndex + 1));
    const calls = vi.mocked(idbDel).mock.calls;
    expect(calls.every(([, store]) => store === calls[0][1])).toBe(true);
    expect(records.has(keys[2])).toBe(true);
    expect(records.has(keys[0])).toBe(failedIndex === 0);
    expect(records.has(keys[1])).toBe(failedIndex < 2);
    expect(localStorage.getItem(ACTIVE_GAME_KEY)).not.toBeNull();
    await expect(loadGameStrict("target")).resolves.toEqual(snapshot);
    expect(idbGet).toHaveBeenLastCalledWith(keys[2], calls[0][1]);
    rejectAt = null;
    vi.mocked(idbDel).mockClear();
    await clearGameStrict("target");
    expect(vi.mocked(idbDel).mock.calls.map(([key]) => key)).toEqual(keys);
    expect(keys.every((key) => !records.has(key))).toBe(true);
    expect(otherKeys.every((key) => records.has(key))).toBe(true);
    expect(localStorage.getItem(ACTIVE_GAME_KEY)).toBeNull();
    await expect(loadGameStrict("target")).resolves.toBeNull();
  });

  it("propagates a strict read error and distinguishes missing from migrated state", async () => {
    const original = new Error("IndexedDB read failed");
    vi.mocked(idbGet).mockRejectedValueOnce(original);
    await expect(loadGameStrict("target")).rejects.toBe(original);
    expect(idbGet).toHaveBeenLastCalledWith(GAME_KEY_PREFIX + "target", expect.anything());
    vi.mocked(idbGet).mockResolvedValueOnce(undefined);
    await expect(loadGameStrict("target")).resolves.toBeNull();
    vi.mocked(idbGet).mockResolvedValueOnce(null as never);
    await expect(loadGameStrict("target")).rejects.toThrow(TypeError);
    const state = fixtureState();
    state.format_config = {
      ...state.format_config,
      format: "CommanderDraft",
      deck_size: 60 as never,
    } as FormatConfig;
    vi.mocked(idbGet).mockResolvedValueOnce(state);
    await expect(loadGameStrict("target")).resolves.toMatchObject({
      format_config: { deck_size: { type: "Minimum", data: 60 } },
    });
    expect((state.format_config as unknown as { deck_size: unknown }).deck_size).toBe(60);
    vi.mocked(idbGet).mockRejectedValueOnce(original);
    await expect(loadGame("target")).resolves.toBeNull();
  });

  it("keeps other active metadata and preserves best-effort cleanup and failed-read behavior", async () => {
    saveActiveGame({ id: "other", mode: "ai", difficulty: "Medium" });
    await clearGameStrict("target");
    expect(localStorage.getItem(ACTIVE_GAME_KEY)).toContain("other");
    saveActiveGame({ id: "target", mode: "ai", difficulty: "Medium" });
    vi.mocked(idbDel).mockRejectedValueOnce(new Error("IDB deletion failed"));
    await expect(clearGame("target")).resolves.toBeUndefined();
    expect(localStorage.getItem(ACTIVE_GAME_KEY)).toBeNull();
    vi.mocked(idbGet).mockRejectedValueOnce(new Error("IDB read failed"));
    await expect(loadGame("target")).resolves.toBeNull();
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

  it("waits for the trusted initial envelope write and propagates its failure", async () => {
    const state = fixtureState();
    const envelope: TrustedGameStateEnvelope = {
      state,
      precast_shortcut_runtime: { opaque: true },
    };
    const adapter = {
      exportPersistenceState: vi.fn().mockResolvedValue(JSON.stringify(envelope)),
    } as unknown as EngineAdapter;
    let settleWrite!: (error?: Error) => void;
    vi.mocked(idbSet).mockImplementation(() => new Promise((resolve, reject) => {
      settleWrite = (error) => error ? reject(error) : resolve();
    }));
    let settled = false;
    const pending = saveAuthoritativeGameStrict("initial", adapter, state);
    void pending.finally(() => { settled = true; }).catch(() => {});
    await vi.waitFor(() => expect(idbSet).toHaveBeenCalledOnce());
    expect(idbSet).toHaveBeenCalledWith(GAME_KEY_PREFIX + "initial", envelope, expect.anything());
    expect(settled).toBe(false);
    settleWrite();
    await expect(pending).resolves.toBeUndefined();
    expect(settled).toBe(true);

    const original = new Error("IndexedDB initial write failed");
    const failed = saveAuthoritativeGameStrict("failed", adapter, state);
    await vi.waitFor(() => expect(idbSet).toHaveBeenCalledTimes(2));
    settleWrite(original);
    await expect(failed).rejects.toBe(original);

    vi.mocked(idbSet).mockRejectedValueOnce(original);
    const warning = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      await expect(saveAuthoritativeGame("ordinary", adapter, state)).resolves.toBeUndefined();
      expect(warning).toHaveBeenCalledWith("[saveGame] IndexedDB write failed:", original);
    } finally {
      warning.mockRestore();
    }
  });

  it("migrates a legacy bare deck size before resume deserialization", async () => {
    const state = fixtureState();
    state.format_config = {
      ...state.format_config,
      format: "Commander",
      command_zone: true,
      deck_size: 100 as never,
    } as FormatConfig;
    vi.mocked(idbGet).mockResolvedValueOnce(state);

    await expect(loadGame("legacy-deck-size")).resolves.toMatchObject({
      format_config: { deck_size: { type: "Exactly", data: 100 } },
    });
  });

  it("uses the Commander Draft registry rule for a legacy command-zone save", async () => {
    const state = fixtureState();
    state.format_config = {
      ...state.format_config,
      format: "CommanderDraft",
      command_zone: true,
      deck_size: 60 as never,
    } as FormatConfig;
    vi.mocked(idbGet).mockResolvedValueOnce(state);

    await expect(loadGame("legacy-commander-draft-deck-size")).resolves.toMatchObject({
      format_config: { deck_size: { type: "Minimum", data: 60 } },
    });
  });

  it("uses engine-authored custom rules instead of command-zone inference", async () => {
    const state = fixtureState();
    state.format_config = {
      ...state.format_config,
      format: "Custom:7",
      command_zone: true,
      deck_size: 60 as never,
      custom_rules: customCommandZoneMinimumRules(),
    } as FormatConfig;
    vi.mocked(idbGet).mockResolvedValueOnce(state);

    await expect(loadGame("legacy-custom-minimum-deck-size")).resolves.toMatchObject({
      format_config: { deck_size: { type: "Minimum", data: 60 } },
    });
  });

  it("fails closed when no format authority can recover a legacy rule", async () => {
    const state = fixtureState();
    state.format_config = {
      ...state.format_config,
      format: "Custom:99",
      command_zone: true,
      deck_size: 60 as never,
      custom_rules: null,
    } as FormatConfig;
    vi.mocked(idbGet).mockResolvedValueOnce(state);

    await expect(loadGame("legacy-unknown-deck-size")).resolves.toBe(state);
    expect((state.format_config as unknown as { deck_size: unknown }).deck_size).toBe(60);
  });

  it("migrates legacy deck sizes when checkpoints are loaded", async () => {
    const checkpoint = fixtureState();
    checkpoint.format_config = {
      ...checkpoint.format_config,
      format: "CommanderDraft",
      command_zone: true,
      deck_size: 60 as never,
    } as FormatConfig;
    vi.mocked(idbGet).mockResolvedValueOnce([checkpoint]);

    await expect(loadCheckpoints("legacy-checkpoints")).resolves.toMatchObject([
      { format_config: { deck_size: { type: "Minimum", data: 60 } } },
    ]);
  });

  it("migrates the state inside a trusted envelope without dropping private fields", () => {
    const state = fixtureState();
    state.format_config = {
      ...state.format_config,
      format: "Standard",
      command_zone: false,
      deck_size: 60 as never,
    } as FormatConfig;
    const envelope = {
      state,
      precast_shortcut_runtime: { opaque: true },
    } as unknown as TrustedGameStateEnvelope;

    expect(migratePersistedGameState(envelope)).toMatchObject({
      state: { format_config: { deck_size: { type: "Minimum", data: 60 } } },
      precast_shortcut_runtime: { opaque: true },
    });
  });

  it("does not clear resumable storage for a terminal state before GameOver delivery", async () => {
    const state = fixtureState();
    state.match_phase = "Completed";

    await saveGame("terminal-pending", state);

    expect(idbSet).not.toHaveBeenCalled();
    expect(idbDel).not.toHaveBeenCalled();
  });

  it("fails closed when asked to retain a terminal state as resumable", async () => {
    const state = fixtureState();
    state.match_phase = "Completed";

    await expect(saveResumableGameStrict("terminal", state)).rejects.toThrow(
      "Refusing to retain a terminal game",
    );
    expect(idbSet).not.toHaveBeenCalled();
  });

  it("propagates a strict resumable-write failure", async () => {
    vi.mocked(idbSet).mockRejectedValueOnce(new Error("IndexedDB unavailable"));

    await expect(saveResumableGameStrict("resume", fixtureState())).rejects.toThrow(
      "IndexedDB unavailable",
    );
  });

  it("rejects a legacy P2P host snapshot without a session key", async () => {
    vi.mocked(idbGet).mockResolvedValueOnce({
      gameId: "legacy-p2p",
      roomCode: "ABCDE",
    });

    await expect(loadP2PHostSession("legacy-p2p")).resolves.toBeNull();
  });
});
