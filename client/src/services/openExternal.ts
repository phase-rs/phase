import { open } from "@tauri-apps/plugin-shell";

import { isTauri } from "./platform";

/**
 * Open an external URL in the user's default browser.
 *
 * A plain `target="_blank"` is unreliable inside a Tauri webview across
 * platforms (tauri#4756, tauri#7285): the webview is the app's only surface, so
 * a stray navigation can replace the SPA with no way back. Under Tauri we route
 * through the shell plugin's `open()` — already permitted via `shell:allow-open`
 * — which hands the URL to the OS and opens the default browser. On the web
 * build there is no Tauri runtime, so we open a new tab as usual.
 */
export function openExternal(url: string): void {
  if (isTauri()) {
    void open(url);
  } else {
    window.open(url, "_blank", "noopener,noreferrer");
  }
}
