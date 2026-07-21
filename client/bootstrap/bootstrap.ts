import { invoke } from "@tauri-apps/api/core";

import { isUserOwnedStorageKey } from "../src/constants/storage";
import { buildBackup } from "../src/services/backup";
import { getSupabaseSessionKey } from "../src/services/cloudSync/sessionKey";
import "./bootstrap.css";

declare const __SHELL_REMOTE_ORIGIN__: string;

const channelUrl = __SHELL_REMOTE_ORIGIN__;
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

async function remoteLoadSucceededBefore(): Promise<boolean> {
  try {
    return await invoke<boolean>("stash_legacy_storage", { json: legacyStashJson() });
  } catch {
    return false;
  }
}

async function navigateToChannel(): Promise<void> {
  retry.hidden = true;
  status.textContent = "Connecting…";

  if (await remoteLoadSucceededBefore()) {
    location.replace(channelUrl);
    return;
  }

  try {
    await fetch(channelUrl, { mode: "no-cors", cache: "no-store" });
    location.replace(channelUrl);
  } catch {
    status.textContent = "Unable to connect.";
    retry.hidden = false;
  }
}

retry.addEventListener("click", () => {
  void navigateToChannel();
});

void navigateToChannel();
