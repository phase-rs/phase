import { buildDeckCatalog } from "../deckCatalog.ts";
import { expandParsedDeck } from "../deckParser.ts";
import { getCachedFeed, listSubscriptions } from "../feedService.ts";
import { loadPrintingsData, loadScryfallData, resolveOracleIdSync } from "../scryfall.ts";
import { loadPreconDeckMap } from "../../hooks/useDecks.ts";
import {
  usePreferencesStore,
  type ArtChainEntry,
  type CardArtOverride,
} from "../../stores/preferencesStore.ts";
import { VisualPackBackendError } from "./backend.ts";
import { planCuratedMembership } from "./curatedMembership.ts";
import type { CuratedDeckPrinting, CuratedMembership } from "./curatedMembership.ts";
import type { PackId } from "./types.ts";

interface DeckLibraryPlanKey {
  readonly packId: PackId;
  readonly artChain: ArtChainEntry[];
  readonly artOverrides: Record<string, CardArtOverride>;
  readonly catalogGeneration: number;
}

interface DeckLibraryPlan {
  readonly key: DeckLibraryPlanKey;
  readonly membership: Promise<CuratedMembership>;
}

let cachedPlan: DeckLibraryPlan | null = null;
let catalogGeneration = 0;

/**
 * Discard the deck-library membership after an input mutation.
 *
 * The generation is part of the key so an invalidation that arrives while a
 * plan is loading card data cannot let that stale promise become a later hit.
 */
export function invalidateDeckLibraryPack(): void {
  catalogGeneration += 1;
  cachedPlan = null;
}

function sameKey(left: DeckLibraryPlanKey, right: DeckLibraryPlanKey): boolean {
  return left.packId === right.packId
    && left.artChain === right.artChain
    && left.artOverrides === right.artOverrides
    && left.catalogGeneration === right.catalogGeneration;
}

function resolvedDeckMembershipInputs(
  candidates: Awaited<ReturnType<typeof buildDeckCatalog>>,
): { includedOracleIds: Set<string>; deckPrintings: CuratedDeckPrinting[] } {
  const includedOracleIds = new Set<string>();
  const deckPrintings = new Map<string, CuratedDeckPrinting>();

  for (const candidate of candidates) {
    const expanded = expandParsedDeck(candidate.deck);
    const names = [
      ...expanded.main_deck,
      ...expanded.sideboard,
      ...expanded.commander,
      ...expanded.planar_deck,
      ...expanded.scheme_deck,
      ...expanded.sticker_sheets,
      ...expanded.signature_spell,
      ...expanded.companion,
    ];
    for (const name of names) {
      const oracleId = resolveOracleIdSync(name);
      if (oracleId) includedOracleIds.add(oracleId);
    }

    for (const entry of [...candidate.deck.main, ...candidate.deck.sideboard]) {
      if (!entry.sourcePrinting) continue;
      const oracleId = resolveOracleIdSync(entry.name);
      if (!oracleId) continue;
      const source = entry.sourcePrinting;
      deckPrintings.set(
        `${oracleId.toLowerCase()}\t${source.setCode}\t${source.collectorNumber}`,
        { oracleId, source },
      );
    }
  }

  return {
    includedOracleIds,
    deckPrintings: [...deckPrintings.values()].sort((left, right) => {
      const leftKey = `${left.oracleId.toLowerCase()}\t${left.source.setCode}\t${left.source.collectorNumber}`;
      const rightKey = `${right.oracleId.toLowerCase()}\t${right.source.setCode}\t${right.source.collectorNumber}`;
      return leftKey < rightKey ? -1 : leftKey > rightKey ? 1 : 0;
    }),
  };
}

async function planMembership(key: DeckLibraryPlanKey): Promise<CuratedMembership> {
  try {
    const [cards, printings, preconDecks] = await Promise.all([
      loadScryfallData(),
      loadPrintingsData(),
      loadPreconDeckMap(),
    ]);
    // No detail: network has a translated user-facing message already.
    if (!cards || !printings || !preconDecks) throw new VisualPackBackendError("network");
    if (listSubscriptions().some((subscription) => !getCachedFeed(subscription.sourceId))) {
      throw new VisualPackBackendError("network");
    }

    // Resolve only after the card data load populates resolveOracleIdSync's
    // synchronous backing map. The catalog intentionally uses its shared
    // defaults so saved, feed, precon, and bundled cEDH candidates agree with
    // the rest of the client.
    const candidates = await buildDeckCatalog();
    const { includedOracleIds, deckPrintings } = resolvedDeckMembershipInputs(candidates);
    return await planCuratedMembership({
      packId: key.packId,
      cards,
      printings,
      artChain: key.artChain,
      artOverrides: key.artOverrides,
      includedOracleIds,
      deckPrintings,
    });
  } catch (error) {
    if (error instanceof VisualPackBackendError) throw error;
    throw new VisualPackBackendError("internal", error instanceof Error ? error.message : undefined);
  }
}

/**
 * Plan the opt-in deck-library pack's current membership.
 *
 * Registration supplies the pack id in a later step; this authority neither
 * creates a selector nor assumes a new id is valid today.
 */
export function planDeckLibraryPack(packId: PackId): Promise<CuratedMembership> {
  const { artChain, artOverrides } = usePreferencesStore.getState();
  const key: DeckLibraryPlanKey = { packId, artChain, artOverrides, catalogGeneration };
  const cached = cachedPlan;
  if (cached && sameKey(cached.key, key)) return cached.membership;

  const entry: DeckLibraryPlan = { key, membership: planMembership(key) };
  cachedPlan = entry;
  // Rejections must be retryable after a transient load failure. Guarding on
  // the entry preserves a newer plan created after invalidation.
  void entry.membership.catch(() => { if (cachedPlan === entry) cachedPlan = null; });
  return entry.membership;
}
