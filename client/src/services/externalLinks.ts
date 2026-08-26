// Tauri webviews silently swallow target=_blank links, so one capture-phase
// handler covers nested content in every current and future anchor without
// per-callsite handlers. Modifier clicks intentionally follow the same path:
// "open in new tab" has no useful meaning inside a webview. Relative app links
// remain with the router; explicit non-HTTP(S) schemes and protocol-relative
// URLs are denied before they can reach the shell.

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
