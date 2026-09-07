import { strFromU8, unzipSync } from "fflate";

import {
  persistedGameStateView,
  type PersistedGameState,
} from "../adapter/types.ts";

/**
 * Parse import text into a `GameState`, or return a human-readable error string.
 *
 * Accepts a bare `GameState`, the full debug-export wrapper
 * (`{ gameState, waitingFor, ... }`), or a trusted persistence envelope
 * (`{ state, ... }`) produced by `gameStateExport.ts`. The presence of
 * `waiting_for` on the resolved public state is the structural marker that
 * distinguishes a game state from arbitrary JSON.
 */
export function gameStateFromImportText(importText: string): PersistedGameState | string {
  let parsed: unknown;
  try {
    parsed = JSON.parse(importText);
  } catch {
    return "Invalid JSON";
  }

  // The debug export nests a raw state under `gameState`; a trusted persistence
  // envelope must stay intact so the engine can restore its private runtime.
  const persistedState = (
    parsed && typeof parsed === "object" && "gameState" in parsed
      ? (parsed as { gameState: PersistedGameState }).gameState
      : parsed
  ) as PersistedGameState;
  if (!persistedState || typeof persistedState !== "object") {
    return "JSON does not look like a GameState (missing waiting_for or players)";
  }
  const state = persistedGameStateView(persistedState);

  if (
    !state
    || typeof state !== "object"
    || !("waiting_for" in state)
    || !Array.isArray(state.players)
  ) {
    return "JSON does not look like a GameState (missing waiting_for or players)";
  }

  return persistedState;
}

/**
 * Read import text from a user-selected file. Plain `.json`/`.txt` files are
 * read directly; `.zip` archives (the format `exportGameStateDebugZip` writes)
 * are unzipped and the first contained JSON/text entry is returned.
 */
export async function readImportFile(file: File): Promise<string> {
  if (!file.name.toLowerCase().endsWith(".zip")) {
    return file.text();
  }

  const archive = unzipSync(new Uint8Array(await file.arrayBuffer()));
  const importFilename = Object.keys(archive).find((name) => {
    const lowerName = name.toLowerCase();
    return lowerName.endsWith(".json") || lowerName.endsWith(".txt");
  });
  if (!importFilename) {
    throw new Error("ZIP does not contain a JSON or text file");
  }

  return strFromU8(archive[importFilename]);
}
