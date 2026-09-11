import { WasmAdapter } from "../adapter/wasm-adapter";
import { useGameStore } from "../stores/gameStore";
import { downloadBlob } from "./fileDownload";

/**
 * Whether the active game has an in-progress replay recording available to
 * export. `false` for non-WASM adapters (online/P2P multiplayer — recording
 * is local/AI-only in v1, see `crates/engine/src/types/replay.rs`) and
 * before any game has started.
 */
export async function hasExportableReplay(): Promise<boolean> {
  const adapter = useGameStore.getState().adapter;
  if (!(adapter instanceof WasmAdapter)) return false;
  const client = adapter.getEngineClient();
  if (!client) return false;
  return client.hasReplayRecording();
}

/**
 * Serialize the active game's replay recording to a JSON string (the format
 * `ReplayAdapter.loadReplay` / `pages/ReplayPage.tsx` read back). Returns
 * `null` when the active adapter isn't a local/AI `WasmAdapter` or no
 * recording is available.
 */
export async function exportCurrentReplayJson(): Promise<string | null> {
  const adapter = useGameStore.getState().adapter;
  if (!(adapter instanceof WasmAdapter)) return null;
  const client = adapter.getEngineClient();
  if (!client) return null;
  if (!(await client.hasReplayRecording())) return null;
  return client.exportReplayLog();
}

/**
 * Export the active game's replay and trigger a browser download. Returns
 * the saved filename, or `null` if no recording was available to export.
 */
export async function downloadCurrentReplay(): Promise<string | null> {
  const json = await exportCurrentReplayJson();
  if (json === null) return null;

  const stamp = new Date().toISOString().replace(/[:.]/g, "-");
  const filename = `phase-replay-${stamp}.json`;
  const blob = new Blob([json], { type: "application/json" });

  return downloadBlob(filename, blob, [
    { description: "Phase replay", accept: { "application/json": [".json"] } },
  ]);
}
