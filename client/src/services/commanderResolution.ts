import type { GameFormat } from "../adapter/types";
import { removeCommandersFromMain, repairParsedDeck, type ParsedDeck } from "./deckParser";
import { inferCommander } from "./engineRuntime";

/**
 * Final, async step of the commander waterfall: after synchronous
 * normalization (explicit sections, inline annotations, the 99 + 1–2
 * sideboard shape), ask the engine to infer a commander for an unmarked list.
 * Send counted main-deck rows and the selected format, when known, to the
 * engine's size, copy-limit and eligibility rules.
 */
export async function resolveCommander(
  deck: ParsedDeck,
  ...format: [selectedFormat?: GameFormat]
): Promise<ParsedDeck> {
  const normalized = repairParsedDeck(deck);
  if (normalized.commander?.length) return normalized;
  if (normalized.sideboard.length > 0) return normalized;
  if (normalized.main.length === 0) return normalized;
  const main = normalized.main.map(({ name, count }) => ({ name, count }));
  let name: string | null;
  try {
    name = await inferCommander(main, ...format);
  } catch {
    // Inference is best-effort: if the engine cannot answer, keep the deck unmarked.
    return normalized;
  }
  return name === null
    ? normalized
    : removeCommandersFromMain({ ...normalized, commander: [name] });
}
