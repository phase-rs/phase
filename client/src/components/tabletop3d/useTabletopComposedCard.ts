import { useEffect, useState } from "react";

import {
  tabletopCardRevision,
  type TabletopCardPresentation,
} from "./tabletopCardPresentation.ts";
import { renderTabletopCardCanvas } from "./tabletopCardCanvas.ts";

interface ComposedCardCacheEntry {
  dataUrl: string | null;
  promise: Promise<string>;
}

const composedCardCache = new Map<string, ComposedCardCacheEntry>();

export function useTabletopComposedCard(
  presentation: TabletopCardPresentation | null,
  artSource: string | null,
): string | null {
  const key =
    presentation && artSource
      ? `${tabletopCardRevision(presentation)}|${artSource}`
      : null;
  const cachedDataUrl = key ? composedCardCache.get(key)?.dataUrl ?? null : null;
  const [resolved, setResolved] = useState<{
    key: string;
    dataUrl: string;
  } | null>(null);

  useEffect(() => {
    if (!key || !presentation || !artSource) {
      setResolved(null);
      return;
    }

    let cancelled = false;
    let entry = composedCardCache.get(key);
    if (!entry) {
      entry = createCacheEntry(presentation, artSource);
      composedCardCache.set(key, entry);
    }
    if (entry.dataUrl) {
      setResolved({ key, dataUrl: entry.dataUrl });
      return;
    }
    entry.promise
      .then((nextDataUrl) => {
        if (!cancelled) setResolved({ key, dataUrl: nextDataUrl });
      })
      .catch((error: unknown) => {
        console.error("Tabletop card composition failed", error);
        if (!cancelled) setResolved(null);
        if (composedCardCache.get(key) === entry) {
          composedCardCache.delete(key);
        }
      });

    return () => {
      cancelled = true;
    };
  }, [artSource, key, presentation]);

  if (cachedDataUrl) return cachedDataUrl;
  return resolved?.key === key ? resolved.dataUrl : null;
}

function createCacheEntry(
  presentation: TabletopCardPresentation,
  artSource: string,
): ComposedCardCacheEntry {
  const entry = {
    dataUrl: null,
  } as ComposedCardCacheEntry;
  entry.promise = renderTabletopCardCanvas(presentation, artSource)
    .then((canvas) => canvas.toDataURL("image/png"))
    .then((nextDataUrl) => {
      entry.dataUrl = nextDataUrl;
      return nextDataUrl;
    });
  return entry;
}
