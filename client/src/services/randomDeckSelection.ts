import type { GameFormat } from "../adapter/types";
import type { DeckCatalogSource } from "./deckCatalog";
import type { DeckCompatibilityResult } from "./deckCompatibility";

export interface RandomDeckCandidate {
  id: string;
  name: string;
  source: DeckCatalogSource;
  knownFormat?: GameFormat;
  compatibility?: Pick<DeckCompatibilityResult, "selected_format_compatible"> | null;
}

export interface RandomDeckPickOptions {
  selectedFormat?: GameFormat | null;
  excludeIds?: ReadonlySet<string>;
  random?: () => number;
}

export function randomDeckPriority(
  candidate: RandomDeckCandidate,
  selectedFormat?: GameFormat | null,
): number {
  if (!selectedFormat) return 0;
  if (candidate.knownFormat === selectedFormat) return 0;
  if (candidate.compatibility?.selected_format_compatible === true) return 1;
  if (candidate.knownFormat && candidate.knownFormat !== selectedFormat) return 4;
  if (candidate.compatibility?.selected_format_compatible === false) return 4;
  return 2;
}

export function topRandomDeckPool<T extends RandomDeckCandidate>(
  candidates: readonly T[],
  options: RandomDeckPickOptions = {},
): T[] {
  const available = options.excludeIds
    ? candidates.filter((candidate) => !options.excludeIds?.has(candidate.id))
    : [...candidates];
  const source = available.length > 0 ? available : [...candidates];
  if (source.length === 0) return [];

  const best = Math.min(
    ...source.map((candidate) => randomDeckPriority(candidate, options.selectedFormat)),
  );
  return source.filter(
    (candidate) => randomDeckPriority(candidate, options.selectedFormat) === best,
  );
}

export function pickRandomDeckCandidate<T extends RandomDeckCandidate>(
  candidates: readonly T[],
  options: RandomDeckPickOptions = {},
): T | null {
  const pool = topRandomDeckPool(candidates, options);
  if (pool.length === 0) return null;
  const random = options.random ?? Math.random;
  return pool[Math.floor(random() * pool.length)];
}
