// Tauri webviews silently swallow target=_blank links, so one capture-phase
// handler covers nested content in every current and future anchor without
// per-callsite handlers. Modifier clicks intentionally follow the same path:
// "open in new tab" has no useful meaning inside a webview. Relative app links
// remain with the router; the page's own blob: download keeps the browser's
// default action, while other non-HTTP(S) schemes and protocol-relative URLs
// are denied before they can reach the shell.

import { isOpenableExternalUrl } from "./openExternal";
import { isBundledTauriOrigin, isTauri } from "./platform";

export const FIRST_PARTY_ORIGINS = new Set([
  "https://phase-rs.dev",
  "https://app.phase-rs.dev",
  "https://preview.phase-rs.dev",
]);

let handlerInstalled = false;

async function openWithOpener(url: string): Promise<void> {
  const { openUrl } = await import("@tauri-apps/plugin-opener");
  await openUrl(url);
}

export function installTauriExternalLinkHandler(): void {
  if (!isTauri() || handlerInstalled) return;
  handlerInstalled = true;

  document.addEventListener(
    "click",
    (event) => {
      if (event.defaultPrevented) return;

      const target = event.target;
      if (!(target instanceof Element)) return;
      const anchor = target.closest("a");
      if (!anchor) return;

      const href = anchor.getAttribute("href");
      if (!href) return;

      // Resolve exactly as the browser does before classifying the destination:
      // it trims leading whitespace and treats backslashes as URL separators.
      // Protocol-relative slash/backslash forms remain denied instead of
      // inheriting the webview's scheme.
      const normalizedHref = href.trim();
      if (/^[\\/]{2}/.test(normalizedHref)) {
        event.preventDefault();
        return;
      }
      let destination: URL;
      try {
        destination = new URL(normalizedHref, window.location.href);
      } catch {
        event.preventDefault();
        return;
      }

      // React Router owns same-origin paths, queries, and fragments.
      if (
        destination.protocol === window.location.protocol &&
        destination.host === window.location.host
      ) {
        return;
      }
      // A blob: href with a download attribute is a file save of the page's own
      // bytes, never a link the shell could open, so leave it to the browser's
      // default action. All three conditions are required: the attribute alone
      // would admit javascript:, which browsers run rather than download; the
      // scheme alone would admit a blob: document the deny path should handle;
      // and a blob: URL carries the origin that created it, so without the
      // origin check blob:https://evil.example/... would take the exemption
      // while being nothing this page ever wrote.
      if (
        destination.protocol === "blob:" &&
        destination.origin === window.location.origin &&
        anchor.hasAttribute("download")
      ) {
        return;
      }
      // Other non-HTTP(S) schemes stay denied, data: among them -- the shell's
      // navigation guard spares only blob:, so admitting data: here would let a
      // download click abort the native-engine and LAN bridges.
      if (!isOpenableExternalUrl(destination.href)) {
        event.preventDefault();
        return;
      }
      if (!isBundledTauriOrigin() && FIRST_PARTY_ORIGINS.has(destination.origin)) return;

      event.preventDefault();
      void openWithOpener(destination.href).catch((err: unknown) => {
        console.warn("[phase.rs] Failed to open external link via Tauri opener.", err);
      });
    },
    { capture: true },
  );
}
