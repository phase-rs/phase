import { strToU8, zipSync } from "fflate";

import type { GameState } from "../adapter/types.ts";
import { useGameStore } from "../stores/gameStore.ts";

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
  await navigator.clipboard.writeText(serializeGameStateDebugSnapshot(gameState, true));
}

export async function exportGameStateDebugZip(gameState: GameState): Promise<string> {
  const stamp = new Date().toISOString().replace(/[:.]/g, "-");
  const baseName = `game-state-turn-${gameState.turn_number}-${stamp}`;
  const jsonFilename = `${baseName}.json`;
  const zipFilename = `${baseName}.zip`;
  const zipped = zipSync(
    { [jsonFilename]: strToU8(serializeGameStateDebugSnapshot(gameState)) },
    { level: 9 },
  );
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
