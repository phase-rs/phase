import { strFromU8, unzipSync } from "fflate";

import {
  persistedGameStateView,
  type PersistedGameState,
} from "../adapter/types.ts";

/**
 * Parse import text into a `GameState`, or return a human-readable error string.
 *
 * Accepts a bare authoritative `GameState` or the trusted persistence envelope
 * (`{ state, ... }`) produced by `gameStateExport.ts`. A debug-export wrapper
 * (`{ gameState, waitingFor, ... }`) is deliberately rejected: it contains the
 * rendered client view, not the private engine runtime needed for restoration.
 */
export function gameStateFromImportText(importText: string): PersistedGameState | string {
  let parsed: unknown;
  try {
    parsed = JSON.parse(importText);
  } catch {
    return "Invalid JSON";
  }

  if (parsed && typeof parsed === "object" && "gameState" in parsed) {
    return "This is a display snapshot, not a restorable game state. Export an Authoritative Game State from the Debug Panel instead.";
  }

  // Keep the trusted envelope intact so the engine can restore its private
  // runtime rather than attempting to rebuild it from a rendered view.
  const persistedState = parsed as PersistedGameState;
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
 * read directly; `.zip` archives are unzipped and the first contained
 * JSON/text entry is returned.
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
