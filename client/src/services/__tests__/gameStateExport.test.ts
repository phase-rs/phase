import { strFromU8, unzipSync } from "fflate";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useGameStore } from "../../stores/gameStore.ts";
import { buildEngineAdapterMock } from "../../test/factories/engineAdapterFactory.ts";
import { buildGameState } from "../../test/factories/gameStateFactory.ts";
import {
  exportGameStateDebugZip,
  exportAuthoritativeGameStateZip,
  serializeGameStateDebugSnapshot,
} from "../gameStateExport.ts";
import { gameStateFromImportText, readImportFile } from "../gameStateImport.ts";

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

  it("imports an authoritative state ZIP without discarding its trusted envelope", async () => {
    const gameState = buildGameState({ turn_number: 7 });
    const trustedEnvelope = {
      state: gameState,
      precast_shortcut_runtime: { nonce: "trusted-runtime" },
    };
    const trustedState = JSON.stringify(trustedEnvelope);
    let writtenBlob: Blob | null = null;
    const write = vi.fn(async (blob: Blob) => {
      writtenBlob = blob;
    });
    Object.defineProperty(window, "showSaveFilePicker", {
      configurable: true,
      value: vi.fn(async () => ({
        createWritable: async () => ({ write, close: vi.fn(async () => {}) }),
      })),
    });
    const adapter = buildEngineAdapterMock(undefined, {
      exportPersistenceState: vi.fn().mockResolvedValue(trustedState),
    });
    useGameStore.setState({ gameMode: "ai" });

    const filename = await exportAuthoritativeGameStateZip(adapter);

    expect(filename).toMatch(/^authoritative-game-state-.*\.zip$/);
    expect(adapter.exportPersistenceState).toHaveBeenCalledOnce();
    const entries = unzipSync(new Uint8Array(await writtenBlob!.arrayBuffer()));
    const [entryName] = Object.keys(entries);
    expect(entryName).toMatch(/^authoritative-game-state-.*\.json$/);
    expect(strFromU8(entries[entryName])).toBe(trustedState);

    const imported = gameStateFromImportText(
      await readImportFile(new File([writtenBlob!], filename, { type: "application/zip" })),
    );
    expect(imported).toEqual(trustedEnvelope);
  });

  it("rejects an incomplete game state import", () => {
    expect(gameStateFromImportText(JSON.stringify({ waiting_for: { type: "Priority" } }))).toBe(
      "JSON does not look like a GameState (missing waiting_for or players)",
    );
  });

  it("exports the trusted envelope from the P2P host", async () => {
    const trustedState = JSON.stringify({ state: { players: [{ hand: ["host-only-card"] }] } });
    const adapter = buildEngineAdapterMock(undefined, {
      exportPersistenceState: vi.fn().mockResolvedValue(trustedState),
    });
    useGameStore.setState({ gameMode: "p2p-host" });

    await exportAuthoritativeGameStateZip(adapter);

    expect(adapter.exportPersistenceState).toHaveBeenCalledOnce();
  });

  it("does not expose the trusted envelope from a P2P guest", async () => {
    const exportPersistenceState = vi.fn().mockResolvedValue(JSON.stringify({
      state: { players: [{ hand: ["hidden-card"] }] },
    }));
    const adapter = buildEngineAdapterMock(undefined, { exportPersistenceState });
    useGameStore.setState({ gameMode: "p2p-join" });

    await expect(exportAuthoritativeGameStateZip(adapter)).rejects.toThrow(
      "Authoritative state export is unavailable for shared games",
    );
    expect(exportPersistenceState).not.toHaveBeenCalled();
  });

  it("falls back to an anchor download when the save picker fails", async () => {
    // Chrome exposes showSaveFilePicker but the picker path can fail there;
    // the export must then degrade to the plain download Firefox uses.
    Object.defineProperty(window, "showSaveFilePicker", {
      configurable: true,
      value: vi.fn(async () => {
        throw new DOMException("The picker is unavailable", "SecurityError");
      }),
    });
    let downloadedBlob: Blob | null = null;
    vi.spyOn(URL, "createObjectURL").mockImplementation((blob) => {
      downloadedBlob = blob as Blob;
      return "blob:mock-url";
    });
    vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => {});
    const clickSpy = vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(() => {});
    const trustedState = JSON.stringify({ state: "trusted-envelope" });
    const adapter = buildEngineAdapterMock(undefined, {
      exportPersistenceState: vi.fn().mockResolvedValue(trustedState),
    });
    useGameStore.setState({ gameMode: "ai" });

    const filename = await exportAuthoritativeGameStateZip(adapter);

    expect(filename).toMatch(/^authoritative-game-state-.*\.zip$/);
    expect(clickSpy).toHaveBeenCalledOnce();
    expect(downloadedBlob).not.toBeNull();
    const entries = unzipSync(new Uint8Array(await downloadedBlob!.arrayBuffer()));
    const [entryName] = Object.keys(entries);
    expect(entryName).toMatch(/^authoritative-game-state-.*\.json$/);
    expect(strFromU8(entries[entryName])).toBe(trustedState);
  });

  it("does not download when the user cancels the save picker", async () => {
    Object.defineProperty(window, "showSaveFilePicker", {
      configurable: true,
      value: vi.fn(async () => {
        throw new DOMException("The user aborted a request", "AbortError");
      }),
    });
    const clickSpy = vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(() => {});
    const adapter = buildEngineAdapterMock(undefined, {
      exportPersistenceState: vi.fn().mockResolvedValue("{}"),
    });
    useGameStore.setState({ gameMode: "ai" });

    const err = await exportAuthoritativeGameStateZip(adapter).catch((e: unknown) => e);

    expect(err).toBeInstanceOf(DOMException);
    expect((err as DOMException).name).toBe("AbortError");
    expect(clickSpy).not.toHaveBeenCalled();
  });
});
