import { beforeEach, describe, expect, it, vi } from "vitest";

import type {
  CustomFormatRules,
  EngineAdapter,
  FormatConfig,
  GameState,
  TrustedGameStateEnvelope,
} from "../../adapter/types";
import { persistedGameStateView } from "../../adapter/types";
import { GAME_KEY_PREFIX } from "../../constants/storage";
import { buildGameState, buildPriorityWaitingFor } from "../../test/factories/gameStateFactory";

vi.mock("idb-keyval", () => ({
  createStore: vi.fn(() => ({})),
  del: vi.fn().mockResolvedValue(undefined),
  get: vi.fn().mockResolvedValue(undefined),
  set: vi.fn().mockResolvedValue(undefined),
}));

import { del as idbDel, get as idbGet, set as idbSet } from "idb-keyval";
import {
  loadGame,
  loadCheckpoints,
  loadP2PHostSession,
  migratePersistedGameState,
  saveAuthoritativeGame,
  saveGame,
  saveResumableGameStrict,
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
