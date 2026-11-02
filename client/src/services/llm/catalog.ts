/**
 * The engine-owned LLM provider catalog, for the settings UI.
 *
 * A stateless query on the shared adapter's engine: the settings modal is
 * usually opened outside a game, and the catalog is a pure constant table
 * (`phase_llm::catalog`) with no game state behind it.
 */

import { getSharedAdapter } from "../../adapter/wasm-adapter";
import type { LlmProviderCatalogEntry } from "./types";

let cached: LlmProviderCatalogEntry[] | null = null;
let inFlight: Promise<LlmProviderCatalogEntry[]> | null = null;

export async function loadProviderCatalog(): Promise<LlmProviderCatalogEntry[]> {
  if (cached) return cached;
  if (inFlight) return inFlight;
  inFlight = (async () => {
    try {
      const rows: unknown = await getSharedAdapter().query("llmProviderCatalog");
      cached = Array.isArray(rows) ? (rows as LlmProviderCatalogEntry[]) : [];
      return cached;
    } catch {
      // A catalog that will not load leaves the free-text provider fields,
      // which are enough to configure any endpoint by hand. Not cached, so a
      // later attempt can still succeed.
      return [];
    } finally {
      inFlight = null;
    }
  })();
  return inFlight;
}
