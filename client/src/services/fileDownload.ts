import { isDesktopTauri } from "./platform";

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
 * What became of a download. Only the desktop shell learns where a
 * browser-style download actually landed, so `path` appears only there, and it
 * arrives home-relative so the page is never told the user's home path. On
 * Windows WebView2 offers `showSaveFilePicker`, so the picker branch runs
 * and the shell wait is never reached.
 */
export type DownloadResult =
  | { kind: "saved"; filename: string; path?: string }
  | { kind: "requested"; filename: string }
  | { kind: "failed"; filename: string; path?: string };

/** Payload of the shell's `shell-download` event (`ShellDownload` in `src-tauri`). */
interface ShellDownload {
  url: string;
  path: string | null;
  outcome: "saved" | "unknown" | "failed";
}

const SHELL_DOWNLOAD_EVENT = "shell-download";

/**
 * How long to wait for the shell to report a finished download before saying
 * only that it was requested. Nothing cancels the download at this point; we
 * just stop claiming to know where it went.
 */
const SHELL_DOWNLOAD_TIMEOUT_MS = 10_000;

/**
 * Click the anchor under the desktop shell and wait for the shell to say where
 * the file landed. The subscription has to exist before the click: a small
 * download can finish before a listener registered afterwards would hear it.
 * `url` is the blob URL being clicked, and it is what tells this download's
 * report apart from a concurrent one's.
 */
async function clickThroughShell(
  filename: string,
  url: string,
  click: () => void,
): Promise<DownloadResult> {
  let settle!: (result: DownloadResult) => void;
  const finished = new Promise<DownloadResult>((resolve) => {
    settle = resolve;
  });
  // Only the setup may fail softly, and only because nothing has been clicked
  // yet: a chunk that would not load must cost the user the destination, never
  // the export itself. Past this point the click has happened exactly once and
  // must not be repeated, so the wait below is deliberately not guarded.
  const unlisten = await import("@tauri-apps/api/event")
    .then(({ listen }) =>
      listen<ShellDownload>(SHELL_DOWNLOAD_EVENT, ({ payload }) => {
        // The event is broadcast to the page, so another export or a log dump
        // finishing inside this wait would otherwise hand us its path. The
        // timeout stays the fallback for a report that never arrives.
        if (payload.url !== url) return;
        settle(
          // The shell reports "unknown" when it cannot tell a written file from
          // a truncated one, which is the same thing we know after the wait runs
          // out: the download was requested and nothing confirmed it.
          payload.outcome === "unknown"
            ? { kind: "requested", filename }
            : { kind: payload.outcome, filename, path: payload.path ?? undefined },
        );
      }),
    )
    .catch(() => null);
  if (!unlisten) {
    click();
    return { kind: "requested", filename };
  }
  const timer = setTimeout(() => settle({ kind: "requested", filename }), SHELL_DOWNLOAD_TIMEOUT_MS);
  try {
    click();
    return await finished;
  } finally {
    clearTimeout(timer);
    unlisten();
  }
}

/**
 * Save a blob to disk, preferring the File System Access "save as" picker
 * when the browser offers one. Opening the picker is known to fail in Chrome
 * while a plain download succeeds, so any picker failure other than the user
 * cancelling falls back to an anchor download.
 *
 * Rejects with an AbortError when the user cancels the picker (callers treat
 * that as "no download", not a failure). Once a destination is chosen, a
 * failure to write it rejects rather than falling back, since the chosen file
 * may already be empty or partial.
 */
export async function downloadBlob(
  filename: string,
  blob: Blob,
  pickerTypes?: SaveFilePickerOptions["types"],
): Promise<DownloadResult> {
  const saveFilePicker = (window as WindowWithSaveFilePicker).showSaveFilePicker;
  if (saveFilePicker) {
    const options: SaveFilePickerOptions = { suggestedName: filename };
    if (pickerTypes) options.types = pickerTypes;
    const handle = await saveFilePicker(options).catch((err: unknown) => {
      // A cancelled picker is deliberate: the user chose not to save, so do
      // not surprise them with a fallback download.
      if (err instanceof DOMException && err.name === "AbortError") throw err;
      // Otherwise the picker failed to open, so no file has been touched
      // yet: fall back to the anchor download below.
      return null;
    });
    if (handle) {
      const writable = await handle.createWritable();
      await writable.write(blob);
      await writable.close();
      // The picker never tells the page which path it wrote.
      return { kind: "saved", filename };
    }
  }

  const url = URL.createObjectURL(blob);
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = filename;
  document.body.appendChild(anchor);
  try {
    // Off the shell nothing can report a destination, so a click is as much as
    // a browser ever knows.
    if (!isDesktopTauri()) {
      anchor.click();
      return { kind: "saved", filename };
    }
    return await clickThroughShell(filename, url, () => anchor.click());
  } finally {
    document.body.removeChild(anchor);
    URL.revokeObjectURL(url);
  }
}
