import { invoke } from "@tauri-apps/api/core";

import { applyBackup, type PhaseBackup } from "./backup";
import { getSupabaseSessionKey } from "./cloudSync/sessionKey";
import { isBundledTauriOrigin, isTauri } from "./platform";

/** Payload staged by the bundled shell for one-time remote-origin import. */
export interface LegacyStorageStash {
  backup: PhaseBackup;
  supabaseSession?: string | null;
}

function isRemoteTauriShell(): boolean {
  return isTauri() && !isBundledTauriOrigin();
}

function parseLegacyStorageStash(json: string): LegacyStorageStash | null {
  try {
    const parsed: unknown = JSON.parse(json);
    if (parsed == null || typeof parsed !== "object" || !("backup" in parsed)) {
      return null;
    }
    return parsed as LegacyStorageStash;
  } catch {
    return null;
  }
}

/**
 * Import the bundled-origin storage staged by the thin shell. Missing commands
 * are expected while a remote deployment rolls out ahead of a new shell.
 */
export async function importLegacyStorage(): Promise<void> {
  if (!isRemoteTauriShell()) return;

  let stashJson: string | null;
  try {
    stashJson = await invoke<string | null>("take_legacy_storage");
  } catch {
    return;
  }
  if (stashJson == null) return;

  const stash = parseLegacyStorageStash(stashJson);
  if (!stash) return;

  try {
    applyBackup(stash.backup, "merge");
    const sessionKey = getSupabaseSessionKey();
    if (
      sessionKey &&
      localStorage.getItem(sessionKey) == null &&
      typeof stash.supabaseSession === "string"
    ) {
      localStorage.setItem(sessionKey, stash.supabaseSession);
    }
  } catch {
    return;
  }

  try {
    await invoke<void>("confirm_legacy_import");
  } catch {
    // A missing command leaves the shell's non-destructive stash for retry.
  }
}

let remoteLoadMarked = false;

/** Mark a completed remote-shell app boot so future offline launches may navigate. */
export function markRemoteLoadOk(): void {
  if (remoteLoadMarked || !isRemoteTauriShell()) return;
  remoteLoadMarked = true;
  void invoke<void>("mark_remote_load_ok").catch(() => {});
}
