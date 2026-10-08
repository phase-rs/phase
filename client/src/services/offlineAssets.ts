import {
  EngineModuleReloadRequiredError,
  ensureCardDatabase,
} from "./engineRuntime.ts";
import { loadScryfallData } from "./scryfall.ts";
import { loadPreconDeckMap } from "../hooks/useDecks.ts";
import { buildDeckCatalog } from "./deckCatalog.ts";
import { BUNDLED_CEDH_DECKS } from "../data/cedhDecks.ts";
import { VisualPackBackendError } from "./visualPacks/backend.ts";
import { prepareDeckLibraryForOffline } from "./visualPacks/deckLibraryAutoSync.ts";
import type { VisualPackErrorKind } from "./visualPacks/types.ts";

export type OfflineAssetCapability =
  | { readonly status: "ready" }
  | { readonly status: "not-ready" };

export type EngineOfflineAssetCapability =
  | { readonly status: "ready"; readonly cardCount: number }
  | { readonly status: "not-ready" }
  | { readonly status: "reload-required" };

export type DeckLibraryOfflineAssetCapability =
  | { readonly status: "ready" }
  | { readonly status: "not-installed" }
  | { readonly status: "not-ready"; readonly error: VisualPackErrorKind };

export interface OfflineAssetsReadiness {
  readonly status: "ready" | "not-ready" | "reload-required";
  readonly capabilities: {
    readonly engine: EngineOfflineAssetCapability;
    readonly scryfallSearch: OfflineAssetCapability;
    readonly preconCatalog: OfflineAssetCapability;
    readonly bundledAiCatalog: OfflineAssetCapability;
    readonly deckLibrary: DeckLibraryOfflineAssetCapability;
  };
}

function hasEntries(value: unknown): value is Record<string, unknown> {
  return value !== null
    && typeof value === "object"
    && !Array.isArray(value)
    && Object.keys(value).length > 0;
}

function hasOwnEntry(value: object, key: string): boolean {
  return Object.prototype.hasOwnProperty.call(value, key);
}

function deckLibraryCapability(
  result: PromiseSettledResult<"ready" | "not-installed">,
): DeckLibraryOfflineAssetCapability {
  if (result.status === "fulfilled") {
    return result.value === "ready" ? { status: "ready" } : { status: "not-installed" };
  }

  return {
    status: "not-ready",
    error: result.reason instanceof VisualPackBackendError ? result.reason.kind : "unavailable",
  };
}

/**
 * Warms the browser data consumed by local play and reports every independently
 * missing capability. The individual authorities own their caches and retries;
 * Deck Catalog preparation remains fresh for each request.
 */
export async function prepareOfflineAssets(): Promise<OfflineAssetsReadiness> {
  const [engineResult, scryfallResult, preconResult, deckLibraryResult] = await Promise.allSettled([
    ensureCardDatabase(),
    loadScryfallData(),
    loadPreconDeckMap(),
    prepareDeckLibraryForOffline(),
  ]);
  const catalogResult = await Promise.allSettled([buildDeckCatalog()]);
  const catalog = catalogResult[0];

  const engine: EngineOfflineAssetCapability = engineResult.status === "fulfilled" && engineResult.value > 0
    ? { status: "ready", cardCount: engineResult.value }
    : engineResult.status === "rejected" && engineResult.reason instanceof EngineModuleReloadRequiredError
      ? { status: "reload-required" }
      : { status: "not-ready" };
  const scryfallSearch: OfflineAssetCapability = scryfallResult.status === "fulfilled" && hasEntries(scryfallResult.value)
    ? { status: "ready" }
    : { status: "not-ready" };
  const preconMap = preconResult.status === "fulfilled" && hasEntries(preconResult.value)
    ? preconResult.value
    : null;
  const deckCatalog = catalog.status === "fulfilled" ? catalog.value : null;
  const preconCatalog: OfflineAssetCapability = preconMap
    && deckCatalog?.some((candidate) => candidate.source.type === "precon"
      && hasOwnEntry(preconMap, candidate.source.deckId))
    ? { status: "ready" }
    : { status: "not-ready" };
  const bundledAiCatalog: OfflineAssetCapability = deckCatalog?.some((candidate) => candidate.source.type === "precon"
    && hasOwnEntry(BUNDLED_CEDH_DECKS, candidate.source.deckId))
    ? { status: "ready" }
    : { status: "not-ready" };
  const deckLibrary = deckLibraryCapability(deckLibraryResult);

  const status = engine.status === "reload-required"
    ? "reload-required"
    : engine.status === "ready"
      && scryfallSearch.status === "ready"
      && preconCatalog.status === "ready"
      && bundledAiCatalog.status === "ready"
      && deckLibrary.status !== "not-ready"
      ? "ready"
      : "not-ready";

  return {
    status,
    capabilities: {
      engine,
      scryfallSearch,
      preconCatalog,
      bundledAiCatalog,
      deckLibrary,
    },
  };
}
