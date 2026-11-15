import {
  looksCommanderSingleton,
  removeCommandersFromMain,
  repairParsedDeck,
  totalCards,
  type ParsedDeck,
} from "./deckParser";
import { isCardCommanderEligible } from "./engineRuntime";

/**
 * Final step in the commander identification waterfall (semantic, async).
 *
 * Steps already applied during sync parsing:
 *   1. Explicit `[Commander]` / `Commander` section header
 *   2. Inline `[Commander]` / `*CMDR*` annotation on a card line
 *   3. 99 main + 1–2 sideboard singletons in a 100-card deck
 *
 * This step:
 *   4. 100-card singleton deck with no commander identified, where the first
 *      commander-eligible card per CR 903.3 (legendary creature, legendary
 *      background, or "can be your commander") is promoted.
 *
 * The eligibility check is delegated to the engine via WASM — the frontend
 * never replicates rules logic.
 */
export async function resolveCommander(deck: ParsedDeck): Promise<ParsedDeck> {
  const normalized = repairParsedDeck(deck);
  if (normalized.commander?.length) return normalized;
  if (normalized.sideboard.length > 0) return normalized;
  if (normalized.main.length === 0) return normalized;
  if (totalCards(normalized.main) !== 100) return normalized;
  if (!looksCommanderSingleton(normalized.main)) return normalized;

  for (const entry of normalized.main) {
    if (entry.count !== 1) continue;
    if (!(await isCardCommanderEligible(entry.name))) continue;

    return removeCommandersFromMain({
      ...normalized,
      commander: [entry.name],
    });
  }

  return normalized;
}
