import { invoke } from "@tauri-apps/api/core";

import { isUserOwnedStorageKey } from "../src/constants/storage";
import { buildBackup } from "../src/services/backup";
import { getSupabaseSessionKey } from "../src/services/cloudSync/sessionKey";
import "./bootstrap.css";

declare const __SHELL_REMOTE_ORIGIN__: string;
declare const __SHELL_PREVIEW_ORIGIN__: string;

type Channel = "release" | "preview";

interface LegacyStorageResult {
  remote_load_ok: boolean;
  channel: Channel;
}

const status = document.querySelector<HTMLParagraphElement>("#status");
const retry = document.querySelector<HTMLButtonElement>("#retry");

if (!status || !retry) {
  throw new Error("Bootstrap page is missing its status controls.");
}

function hasLegacyStorage(sessionKey: string | null): boolean {
  for (let index = 0; index < localStorage.length; index += 1) {
    const key = localStorage.key(index);
    if (key && (isUserOwnedStorageKey(key) || key === sessionKey)) return true;
  }
  return false;
}

function legacyStashJson(): string {
  const sessionKey = getSupabaseSessionKey();
  if (!hasLegacyStorage(sessionKey)) return "";

  return JSON.stringify({
    backup: buildBackup(),
    supabaseSession: sessionKey ? localStorage.getItem(sessionKey) : null,
  });
}

function channelUrl(channel: Channel): string {
  return channel === "preview" ? __SHELL_PREVIEW_ORIGIN__ : __SHELL_REMOTE_ORIGIN__;
}

async function stashLegacyStorage(): Promise<LegacyStorageResult> {
  try {
    return await invoke<LegacyStorageResult>("stash_legacy_storage", { json: legacyStashJson() });
  } catch {
    return { remote_load_ok: false, channel: "release" };
  }
}

async function navigateToChannel(): Promise<void> {
  retry.hidden = true;
  status.textContent = "Connecting…";
  const { remote_load_ok: remoteLoadOk, channel } = await stashLegacyStorage();
  const destination = channelUrl(channel);

  if (remoteLoadOk) {
    location.replace(destination);
    return;
  }

  try {
    await fetch(destination, { mode: "no-cors", cache: "no-store" });
    location.replace(destination);
  } catch {
    status.textContent = "Unable to connect.";
    retry.hidden = false;
  }
}

retry.addEventListener("click", () => {
  void navigateToChannel();
});

void navigateToChannel();
