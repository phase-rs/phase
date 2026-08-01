import { invoke } from "@tauri-apps/api/core";

import { isBundledTauriOrigin, isTauri } from "./platform";

/** Remember a first-party shell's selected remote content channel. */
export function rememberChannelPreference(channel: "release" | "preview"): void {
  if (!isTauri() || isBundledTauriOrigin()) return;

  void invoke<void>("set_channel_preference", { channel }).catch(() => {});
}
