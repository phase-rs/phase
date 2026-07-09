/** Minimal deck shape the bracket-estimate key depends on. */
export interface BracketKeyEntry {
  name: string;
  count: number;
}
export interface BracketKeyDeck {
  main: BracketKeyEntry[];
  sideboard: BracketKeyEntry[];
}

/**
 * Content key for a bracket-estimate request. Two decks produce the same key
 * iff they would produce the same estimate, so the hook can cache/short-circuit
 * on it. It must therefore cover **everything** the estimate depends on —
 * commanders, main, AND sideboard (the request sends the sideboard too). The
 * parts are sorted so object-identity / ordering churn does not change the key.
 */
export function buildBracketDeckKey(
  commanders: string[],
  deck: BracketKeyDeck,
): string {
  const parts: string[] = [...commanders.map((c) => `c:${c.toLowerCase()}`)];
  for (const e of deck.main) parts.push(`m:${e.count}x${e.name.toLowerCase()}`);
  for (const e of deck.sideboard) parts.push(`s:${e.count}x${e.name.toLowerCase()}`);
  parts.sort();
  return parts.join("|");
}
