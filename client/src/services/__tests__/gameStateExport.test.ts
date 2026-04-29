import { strFromU8, unzipSync } from "fflate";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { GameState } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import {
  exportGameStateDebugZip,
  serializeGameStateDebugSnapshot,
} from "../gameStateExport.ts";

function createGameState(overrides: Partial<GameState> = {}): GameState {
  return {
    turn_number: 7,
    active_player: 0,
    phase: "PreCombatMain",
    players: [
      { id: 0, life: 20, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0, turns_taken: 0 },
      { id: 1, life: 20, poison_counters: 0, mana_pool: { mana: [] }, library: [], hand: [], graveyard: [], has_drawn_this_turn: false, lands_played_this_turn: 0, turns_taken: 0 },
    ],
    priority_player: 0,
    objects: {},
    next_object_id: 1,
    battlefield: [],
    stack: [],
    exile: [],
    rng_seed: 1,
    combat: null,
    waiting_for: { type: "Priority", data: { player: 0 } },
    has_pending_cast: false,
    lands_played_this_turn: 0,
    max_lands_per_turn: 1,
    priority_pass_count: 0,
    pending_replacement: null,
    layers_dirty: false,
    next_timestamp: 1,
    ...overrides,
  } as GameState;
}

describe("gameStateExport", () => {
  afterEach(() => {
    vi.restoreAllMocks();
    Reflect.deleteProperty(window, "showSaveFilePicker");
  });

  it("serializes the debug snapshot as minified JSON by default", () => {
    const gameState = createGameState();
    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
      legalActions: [{ type: "PassPriority" }],
      turnCheckpoints: [gameState],
    });

    const serialized = serializeGameStateDebugSnapshot(gameState);

    expect(serialized).not.toContain("\n");
    expect(JSON.parse(serialized)).toMatchObject({
      gameState: { turn_number: 7 },
      waitingFor: { type: "Priority" },
      legalActions: [{ type: "PassPriority" }],
      turnCheckpoints: [{ turn_number: 7 }],
    });
  });

  it("writes a zip containing the minified JSON snapshot through the save picker", async () => {
    const gameState = createGameState();
    let writtenBlob: Blob | null = null;
    const write = vi.fn(async (blob: Blob) => {
      writtenBlob = blob;
    });
    const close = vi.fn(async () => {});
    Object.defineProperty(window, "showSaveFilePicker", {
      configurable: true,
      value: vi.fn(async () => ({
        createWritable: async () => ({ write, close }),
      })),
    });
    useGameStore.setState({
      gameState,
      waitingFor: gameState.waiting_for,
      legalActions: [],
      turnCheckpoints: [],
    });

    const filename = await exportGameStateDebugZip(gameState);

    expect(filename).toMatch(/^game-state-turn-7-.*\.zip$/);
    expect(write).toHaveBeenCalledOnce();
    expect(close).toHaveBeenCalledOnce();
    expect(writtenBlob).not.toBeNull();

    const zipped = new Uint8Array(await writtenBlob!.arrayBuffer());
    const entries = unzipSync(zipped);
    const [entryName] = Object.keys(entries);
    const json = strFromU8(entries[entryName]);

    expect(entryName).toMatch(/^game-state-turn-7-.*\.json$/);
    expect(json).not.toContain("\n");
    expect(JSON.parse(json).gameState.turn_number).toBe(7);
  });
});
