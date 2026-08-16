import { strFromU8, unzipSync } from "fflate";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useGameStore } from "../../stores/gameStore.ts";
import { buildGameState } from "../../test/factories/gameStateFactory.ts";
import {
  exportGameStateDebugZip,
  serializeGameStateDebugSnapshot,
} from "../gameStateExport.ts";

describe("gameStateExport", () => {
  afterEach(() => {
    vi.restoreAllMocks();
    Reflect.deleteProperty(window, "showSaveFilePicker");
  });

  it("serializes the debug snapshot as minified JSON by default", () => {
    const gameState = buildGameState({ turn_number: 7 });
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
    const gameState = buildGameState({ turn_number: 7 });
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
