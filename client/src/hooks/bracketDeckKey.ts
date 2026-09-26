import type { CommanderBracketTier } from "../types/bracketEstimate";

/** Minimal deck shape the bracket-estimate key depends on. */
export interface BracketKeyEntry {
  name: string;
  count: number;
}
export interface BracketKeyDeck {
  main: BracketKeyEntry[];
  sideboard: BracketKeyEntry[];
  /** ParsedDeck carries at most one companion. */
  companion?: string;
  /** Oathbreaker RC: 0 or 1 entries. */
  signature_spell?: string[];
}

/**
 * Content key for a bracket-estimate request. The key must be a SUPERSET of the
 * estimate's inputs: two decks with the same key must produce the same estimate.
 * Over-keying is safe (an extra cache miss); under-keying returns another deck's
 * estimate. The sideboard is therefore kept in the key even though
 * `estimate_bracket` does not read it — Commander has no sideboard, and dropping
 * it would buy nothing while making the key wrong the day a section is added.
 * Companion and signature spell ARE read by the estimator and must be in the key.
 * The declaration is also an input to the estimate and must be in the key.
 */
export function buildBracketDeckKey(
  commanders: string[],
  deck: BracketKeyDeck,
  declaredTier: CommanderBracketTier | null,
): string {
  const parts: string[] = [...commanders.map((c) => `c:${c.toLowerCase()}`)];
  for (const e of deck.main) parts.push(`m:${e.count}x${e.name.toLowerCase()}`);
  for (const e of deck.sideboard) parts.push(`s:${e.count}x${e.name.toLowerCase()}`);
  if (deck.companion) parts.push(`co:${deck.companion.toLowerCase()}`);
  for (const name of deck.signature_spell ?? []) parts.push(`sig:${name.toLowerCase()}`);
  parts.sort();
  // Appended AFTER the sort so it is a suffix, not an interleaved part: the
  // declaration is an input to the estimate (it produces `declaration`), not a
  // deck member.
  return `${parts.join("|")}#d:${declaredTier ?? "none"}`;
}
