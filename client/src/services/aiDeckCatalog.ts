import { useEffect, useState } from "react";

import type { GameFormat, MatchType } from "../adapter/types";
import { evaluateDeckCompatibility } from "./deckCompatibility";
import {
  buildDeckCatalog,
  savedDeckCatalogId,
  type DeckCatalogSource,
} from "./deckCatalog";
import type { DeckArchetype } from "./engineRuntime";
import type { ParsedDeck } from "./deckParser";

export type AiDeckSource = DeckCatalogSource;

export interface AiDeckCandidate {
  id: string;
  name: string;
  source: AiDeckSource;
  deck: ParsedDeck;
  coveragePct: number | null;
  archetype: DeckArchetype | null;
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

async function legalCandidate(
  candidate: AiDeckCandidate & { knownFormat?: GameFormat },
  options: AiDeckCatalogOptions,
): Promise<AiDeckCandidate | null> {
  const { knownFormat, ...base } = candidate;
  if (knownFormat && options.selectedFormat && knownFormat !== options.selectedFormat) return null;
  if (candidate.source.type === "precon" && knownFormat) return base;

  const result = await evaluateDeckCompatibility(candidate.deck, {
    selectedFormat: options.selectedFormat,
    selectedMatchType: options.selectedMatchType,
    summaryOnly: true,
  });
  if (result.selected_format_compatible !== true) return null;
  return {
    ...base,
    coveragePct: result.coverage && result.coverage.total_unique > 0
      ? Math.round((result.coverage.supported_unique / result.coverage.total_unique) * 100)
      : base.coveragePct,
  };
}

export function legacyAiDeckNameToId(name: string): string {
  return savedDeckCatalogId(name);
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
    knownFormat: candidate.knownFormat,
  }));

  const legal = await Promise.all(
    rawCandidates.map((candidate) => legalCandidate(candidate, options)),
  );
  return { candidates: legal.filter((candidate): candidate is AiDeckCandidate => candidate !== null) };
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
