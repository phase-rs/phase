import { useEffect, useState } from "react";

import type { GameFormat, MatchType } from "../adapter/types";
import { estimateDeckBracket } from "./bracketEstimate";
import { evaluateDeckCompatibility } from "./deckCompatibility";
import {
  buildDeckCatalog,
  savedDeckCatalogId,
  type DeckCatalogSource,
} from "./deckCatalog";
import type { DeckArchetype } from "./engineRuntime";
import { expandParsedDeck, type ParsedDeck } from "./deckParser";
import type { CommanderBracket } from "../types/bracket";
import {
  BRACKET_TIER_NUMERIC,
  isCommanderFamilyFormat,
  UNDECLARED_COMBO,
} from "../types/bracket";

export type AiDeckSource = DeckCatalogSource;

export interface AiDeckCandidate {
  id: string;
  name: string;
  source: AiDeckSource;
  deck: ParsedDeck;
  knownFormat?: GameFormat;
  coveragePct: number | null;
  archetype: DeckArchetype | null;
  bracket: CommanderBracket | null;
  bracketProvenance: "declared" | "estimated" | null;
  bracketDataVersion: string | null;
}

export interface AiDeckCatalogOptions {
  selectedFormat?: GameFormat | null;
  selectedMatchType?: MatchType | null;
}

export interface AiDeckCatalogResult {
  candidates: AiDeckCandidate[];
}

export interface UseAiDeckCatalogResult extends AiDeckCatalogResult {
  loading: boolean;
  error: string | null;
}

/**
 * Resolve a candidate's bracket tier for the AI random-pool filter.
 *
 * Prefers an explicit human-declared tag (a curated precon entry, a bundled
 * cEDH deck, or a user-saved bracket) when one exists. Commander declarations
 * still request an engine estimate: its data version lets the selector verify
 * that the declaration is current, while its tier remains advisory metadata.
 * Untagged decks use the estimate's tier as well as its data version.
 *
 * Pre-game metadata only: the value filters the candidate pool and never
 * reaches the Rust game loop. The estimate is meaningful only for the
 * Commander family (CR 903), so non-Commander formats and decks with no
 * commander short-circuit to `null`.
 */
async function resolveBracket(
  deck: ParsedDeck,
  staticBracket: CommanderBracket | null,
  format: GameFormat | undefined,
): Promise<Pick<AiDeckCandidate, "bracket" | "bracketProvenance" | "bracketDataVersion">> {
  if (!isCommanderFamilyFormat(format)) {
    return {
      bracket: staticBracket,
      bracketProvenance: staticBracket === null ? null : "declared",
      bracketDataVersion: null,
    };
  }
  const request = expandParsedDeck(deck);
  if (request.commander.length === 0) {
    return {
      bracket: staticBracket,
      bracketProvenance: staticBracket === null ? null : "declared",
      bracketDataVersion: null,
    };
  }
  const outcome = await estimateDeckBracket({
    deck: { ...request, combo_declaration: UNDECLARED_COMBO },
    declared_tier: null,
  });
  switch (outcome.kind) {
    case "estimate":
      return {
        bracket: staticBracket ?? BRACKET_TIER_NUMERIC[outcome.estimate.tier],
        bracketProvenance: staticBracket === null ? "estimated" : "declared",
        bracketDataVersion: outcome.estimate.data_version,
      };
    case "no-commander":
    case "card-data-unavailable":
      // Preserve a static tier, but do not invent a data version. The wire
      // conversion will consequently submit it with estimated provenance.
      return {
        bracket: staticBracket,
        bracketProvenance: staticBracket === null ? null : "declared",
        bracketDataVersion: null,
      };
  }
}

async function legalCandidate(
  candidate: AiDeckCandidate,
  options: AiDeckCatalogOptions,
): Promise<AiDeckCandidate | null> {
  const { knownFormat } = candidate;
  if (knownFormat && options.selectedFormat && knownFormat !== options.selectedFormat) return null;

  // Precon decks MUST still pass the legality check (CR 903 + the Commander
  // Rules Committee ban list). WotC ships precons with cards that later get
  // banned (Jeweled Lotus, Mana Crypt, Dockside Extortionist in 2024+) and
  // never retroactively curates the precon lists. The previous short-circuit
  // "if precon, skip compat" let AI opponents auto-pick decks containing
  // banned cards — the engine is the rules authority, no catalog bypass.
  const result = await evaluateDeckCompatibility(candidate.deck, {
    selectedFormat: options.selectedFormat,
    selectedMatchType: options.selectedMatchType,
    summaryOnly: true,
  });
  if (result.selected_format_compatible !== true) return null;
  const bracket = await resolveBracket(
    candidate.deck,
    candidate.bracket,
    options.selectedFormat ?? undefined,
  );
  return {
    ...candidate,
    ...bracket,
    coveragePct: result.coverage && result.coverage.total_unique > 0
      ? Math.round((result.coverage.supported_unique / result.coverage.total_unique) * 100)
      : candidate.coveragePct,
  };
}

export function legacyAiDeckNameToId(name: string): string {
  return savedDeckCatalogId(name);
}

/**
 * Pure bracket filter for AI deck candidates.
 *
 * - `tier === null` — no constraint; returns all candidates unchanged.
 * - `tier !== null` — returns only candidates whose `bracket` equals `tier`.
 *   Untagged candidates (`bracket === null`) are excluded.
 */
export function filterByBracket(
  decks: AiDeckCandidate[],
  tier: CommanderBracket | null,
): AiDeckCandidate[] {
  if (tier === null) return decks;
  return decks.filter((d) => d.bracket === tier);
}

export async function buildLegalAiDeckCatalog(
  options: AiDeckCatalogOptions,
): Promise<AiDeckCatalogResult> {
  const rawCandidates = (await buildDeckCatalog()).map((candidate) => ({
    id: candidate.id,
    name: candidate.name,
    source: candidate.source,
    deck: candidate.deck,
    coveragePct: candidate.coveragePct ?? null,
    archetype: null,
    bracket: candidate.bracket ?? null,
    bracketProvenance: null,
    bracketDataVersion: null,
    knownFormat: candidate.knownFormat,
  }));

  // The shared engine worker owns a full card database. Issuing the complete
  // catalog at once fills its message queue with cloned deck lists before the
  // worker can service any of them, which can exhaust WebKit's process budget
  // while the setup page opens. Keep only one compatibility request in flight.
  const candidates: AiDeckCandidate[] = [];
  for (const candidate of rawCandidates) {
    const legalCandidateResult = await legalCandidate(candidate, options);
    if (legalCandidateResult !== null) candidates.push(legalCandidateResult);
  }
  return { candidates };
}

export function useAiDeckCatalog({
  selectedFormat,
  selectedMatchType,
}: AiDeckCatalogOptions): UseAiDeckCatalogResult {
  const [result, setResult] = useState<UseAiDeckCatalogResult>({
    candidates: [],
    loading: true,
    error: null,
  });

  useEffect(() => {
    let cancelled = false;
    setResult((current) => ({ ...current, loading: true, error: null }));
    buildLegalAiDeckCatalog({ selectedFormat, selectedMatchType })
      .then((catalog) => {
        if (!cancelled) setResult({ ...catalog, loading: false, error: null });
      })
      .catch((error) => {
        if (!cancelled) {
          setResult({
            candidates: [],
            loading: false,
            error: error instanceof Error ? error.message : String(error),
          });
        }
      });
    return () => {
      cancelled = true;
    };
  }, [selectedFormat, selectedMatchType]);

  return result;
}
