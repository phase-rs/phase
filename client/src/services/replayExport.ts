import { WasmAdapter } from "../adapter/wasm-adapter";
import { useGameStore } from "../stores/gameStore";

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

  const saveFilePicker = (window as WindowWithSaveFilePicker).showSaveFilePicker;
  if (saveFilePicker) {
    const handle = await saveFilePicker({
      suggestedName: filename,
      types: [
        {
          description: "Phase replay",
          accept: { "application/json": [".json"] },
        },
      ],
    });
    const writable = await handle.createWritable();
    await writable.write(blob);
    await writable.close();
    return filename;
  }

  const url = URL.createObjectURL(blob);
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = filename;
  document.body.appendChild(anchor);
  anchor.click();
  document.body.removeChild(anchor);
  URL.revokeObjectURL(url);
  return filename;
}
