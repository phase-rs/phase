import { strFromU8, unzipSync } from "fflate";

import type { GameState } from "../adapter/types.ts";

/**
 * Parse import text into a `GameState`, or return a human-readable error string.
 *
 * Accepts either a bare `GameState` or the full debug-export wrapper
 * (`{ gameState, waitingFor, ... }`) produced by `gameStateExport.ts`. The
 * presence of `waiting_for` on the resolved object is the structural marker
 * that distinguishes a GameState from arbitrary JSON.
 */
export function gameStateFromImportText(importText: string): GameState | string {
  let parsed: unknown;
  try {
    parsed = JSON.parse(importText);
  } catch {
    return "Invalid JSON";
  }

  // Accept either a bare GameState or the full debug export format {gameState, ...}
  const state = (
    parsed && typeof parsed === "object" && "gameState" in parsed
      ? (parsed as { gameState: GameState }).gameState
      : parsed
  ) as GameState;

  if (!state || typeof state !== "object" || !("waiting_for" in state)) {
    return "JSON does not look like a GameState (missing waiting_for)";
  }

  return state;
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
