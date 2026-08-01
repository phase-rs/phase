// External link routing for Tauri.
//
// In a regular browser, `<a target="_blank">` opens a new tab. Inside the
// Tauri webview that target is silently swallowed — the webview has no
// concept of "open in user's default browser" without explicit shell
// integration. This installs a single document-level click capture that
// intercepts external (http/https) link clicks in Tauri and hands them off
// to `@tauri-apps/plugin-shell`'s `open()`, which invokes the OS handler
// (xdg-open / `open` / ShellExecute).
//
// Capture phase + closest("a") rather than per-callsite onClick handlers so
// every existing and future external link in the app works without changes.
// The release and preview origins stay inside a remote-origin shell so channel
// navigation keeps its service worker and Tauri IPC context.

import { isBundledTauriOrigin, isTauri } from "./platform";

const EXTERNAL_URL_RE = /^https?:\/\//i;
const FIRST_PARTY_ORIGINS = new Set([
  "https://phase-rs.dev",
  "https://preview.phase-rs.dev",
]);

async function openWithShell(url: string): Promise<void> {
  const { open } = await import("@tauri-apps/plugin-shell");
  await open(url);
}

export function installTauriExternalLinkHandler(): void {
  if (!isTauri()) return;

  document.addEventListener(
    "click",
    (event) => {
      // Modifier-clicks (cmd/ctrl/shift/middle) are still meaningless in
      // Tauri — there's nowhere to "open in new tab" — so route them too.
      if (event.defaultPrevented) return;

      const target = event.target;
      if (!(target instanceof Element)) return;
      const anchor = target.closest("a");
      if (!anchor) return;

      const href = anchor.getAttribute("href");
      if (!href || !EXTERNAL_URL_RE.test(href)) return;
      if (!isBundledTauriOrigin() && FIRST_PARTY_ORIGINS.has(new URL(href).origin)) return;

      event.preventDefault();
      void openWithShell(href).catch((err: unknown) => {
        console.warn("[phase.rs] Failed to open external link via Tauri shell.", err);
      });
    },
    { capture: true },
  );
}
