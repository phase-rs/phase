import { removeCommandersFromMain, repairParsedDeck, type ParsedDeck } from "./deckParser";
import { inferCommander } from "./engineRuntime";

/**
 * Final, async step of the commander waterfall: after synchronous
 * normalization (explicit sections, inline annotations, the 99 + 1–2
 * sideboard shape), ask the engine to infer a commander for an unmarked list.
 * The engine owns the size, copy-limit and eligibility rules; this only sends
 * the counted main-deck rows and applies the answer.
 */
export async function resolveCommander(deck: ParsedDeck): Promise<ParsedDeck> {
  const normalized = repairParsedDeck(deck);
  if (normalized.commander?.length) return normalized;
  if (normalized.sideboard.length > 0) return normalized;
  if (normalized.main.length === 0) return normalized;
  const main = normalized.main.map(({ name, count }) => ({ name, count }));
  let name: string | null;
  try {
    name = await inferCommander(main);
  } catch {
    // Inference is best-effort: if the engine cannot answer, keep the deck unmarked.
    return normalized;
  }
  return name === null
    ? normalized
    : removeCommandersFromMain({ ...normalized, commander: [name] });
}
