import type { VisualPackBackend } from "./backend.ts";
import { ScryfallBrowserVisualPackBackend } from "./browser/scryfallBackend.ts";

export async function createVisualPackBackend(_isTauri: boolean): Promise<VisualPackBackend | null> {
  if (typeof indexedDB === "undefined" || typeof caches === "undefined") {
    return null;
  }
  return ScryfallBrowserVisualPackBackend.create();
}
