import { useEffect, useState } from "react";

import { loadProviderCatalog } from "../services/llm/catalog";
import type { LlmProviderCatalogEntry } from "../services/llm/types";

/** Publish the engine catalog when it arrives so provider choices rerender. */
export function useLlmProviderCatalog(): LlmProviderCatalogEntry[] {
  const [catalog, setCatalog] = useState<LlmProviderCatalogEntry[]>([]);

  useEffect(() => {
    let cancelled = false;
    void loadProviderCatalog().then((rows) => {
      if (!cancelled) setCatalog(rows);
    });
    return () => {
      cancelled = true;
    };
  }, []);

  return catalog;
}
