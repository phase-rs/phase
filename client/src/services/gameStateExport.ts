import { strToU8, zipSync } from "fflate";

import type { EngineAdapter, GameState } from "../adapter/types.ts";
import { canExportAuthoritativeState, useGameStore } from "../stores/gameStore.ts";
import { copyText } from "./copyText";

interface GameStateDebugSnapshot {
  gameState: GameState;
  waitingFor: GameState["waiting_for"];
  legalActions: ReturnType<typeof useGameStore.getState>["legalActions"];
  turnCheckpoints: ReturnType<typeof useGameStore.getState>["turnCheckpoints"];
}

interface FileSystemWritableFileStream {
  write: (data: Blob) => Promise<void>;
  close: () => Promise<void>;
}

interface FileSystemFileHandle {
  createWritable: () => Promise<FileSystemWritableFileStream>;
}

interface SaveFilePickerOptions {
  suggestedName?: string;
  types?: Array<{
    description: string;
    accept: Record<string, string[]>;
  }>;
}

type WindowWithSaveFilePicker = Window & {
  showSaveFilePicker?: (options?: SaveFilePickerOptions) => Promise<FileSystemFileHandle>;
};

async function downloadZip(baseName: string, contents: Record<string, Uint8Array>): Promise<string> {
  const zipFilename = `${baseName}.zip`;
  const zipped = zipSync(contents, { level: 9 });
  const blob = new Blob([zipped as BlobPart], { type: "application/zip" });

  const saveFilePicker = (window as WindowWithSaveFilePicker).showSaveFilePicker;
  if (saveFilePicker) {
    const handle = await saveFilePicker({
      suggestedName: zipFilename,
      types: [
        {
          description: "ZIP archive",
          accept: { "application/zip": [".zip"] },
        },
      ],
    });
    const writable = await handle.createWritable();
    await writable.write(blob);
    await writable.close();
    return zipFilename;
  }

  const url = URL.createObjectURL(blob);
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = zipFilename;
  document.body.appendChild(anchor);
  anchor.click();
  document.body.removeChild(anchor);
  URL.revokeObjectURL(url);
  return zipFilename;
}

export function buildGameStateDebugSnapshot(gameState: GameState): GameStateDebugSnapshot {
  const store = useGameStore.getState();
  return {
    gameState,
    waitingFor: gameState.waiting_for,
    legalActions: store.legalActions,
    turnCheckpoints: store.turnCheckpoints,
  };
}

export function serializeGameStateDebugSnapshot(gameState: GameState, pretty = false): string {
  return JSON.stringify(buildGameStateDebugSnapshot(gameState), null, pretty ? 2 : undefined);
}

export async function copyGameStateDebugSnapshot(gameState: GameState): Promise<void> {
  if (!(await copyText(serializeGameStateDebugSnapshot(gameState, true)))) {
    throw new Error("Clipboard write failed");
  }
}

export async function exportGameStateDebugZip(gameState: GameState): Promise<string> {
  const stamp = new Date().toISOString().replace(/[:.]/g, "-");
  const baseName = `game-state-turn-${gameState.turn_number}-${stamp}`;
  const jsonFilename = `${baseName}.json`;
  return downloadZip(
    baseName,
    { [jsonFilename]: strToU8(serializeGameStateDebugSnapshot(gameState)) },
  );
}

/**
 * Exports the engine-owned persistence envelope for a bug report. Unlike a
 * rendered client snapshot, this retains private runtime state needed to
 * diagnose and restore the game accurately.
 */
export async function exportAuthoritativeGameStateZip(adapter: EngineAdapter): Promise<string> {
  if (!canExportAuthoritativeState(useGameStore.getState().gameMode)) {
    throw new Error("Authoritative state export is unavailable for shared games");
  }
  if (!adapter.exportPersistenceState) {
    throw new Error("The current game authority cannot export a trusted state");
  }

  const stamp = new Date().toISOString().replace(/[:.]/g, "-");
  const baseName = `authoritative-game-state-${stamp}`;
  const jsonFilename = `${baseName}.json`;
  const trustedState = await adapter.exportPersistenceState();
  return downloadZip(
    baseName,
    { [jsonFilename]: strToU8(trustedState) },
  );
}
