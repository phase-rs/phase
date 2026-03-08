export interface DeckEntry {
  count: number;
  name: string;
}

export interface ParsedDeck {
  main: DeckEntry[];
  sideboard: DeckEntry[];
}

/**
 * Parse a .dck/.dec format deck file.
 * Format: "count CardName" per line (or "countx CardName").
 * Sections: [Main], [Sideboard] (case-insensitive).
 * Lines starting with # are comments, empty lines are skipped.
 */
export function parseDeckFile(content: string): ParsedDeck {
  const lines = content.split(/\r?\n/);
  const deck: ParsedDeck = { main: [], sideboard: [] };
  let currentSection: "main" | "sideboard" = "main";

  for (const raw of lines) {
    const line = raw.trim();
    if (!line || line.startsWith("#")) continue;

    const sectionMatch = line.match(/^\[(\w+)\]$/i);
    if (sectionMatch) {
      currentSection =
        sectionMatch[1].toLowerCase() === "sideboard" ? "sideboard" : "main";
      continue;
    }

    const cardMatch = line.match(/^(\d+)x?\s+(.+)$/);
    if (cardMatch) {
      deck[currentSection].push({
        count: parseInt(cardMatch[1], 10),
        name: cardMatch[2].trim(),
      });
    }
  }

  return deck;
}

/**
 * Export a ParsedDeck to .dck format string.
 */
export function exportDeckFile(deck: ParsedDeck): string {
  const lines: string[] = [];

  if (deck.main.length > 0) {
    lines.push("[Main]");
    for (const entry of deck.main) {
      lines.push(`${entry.count} ${entry.name}`);
    }
  }

  if (deck.sideboard.length > 0) {
    lines.push("[Sideboard]");
    for (const entry of deck.sideboard) {
      lines.push(`${entry.count} ${entry.name}`);
    }
  }

  return lines.join("\n") + "\n";
}
