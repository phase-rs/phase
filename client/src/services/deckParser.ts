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

const MTGA_LINE_PATTERN = /^\d+\s+.+\s+\([A-Z0-9]+\)\s+\d+$/;

/**
 * Parse an MTGA text format deck.
 * Format: "count CardName (SET) CollectorNumber" per line.
 * A blank line or "Sideboard" header switches to sideboard section.
 * Header labels like "Deck", "Companion" are skipped.
 */
export function parseMtgaDeck(content: string): ParsedDeck {
  const lines = content.split(/\r?\n/);
  const deck: ParsedDeck = { main: [], sideboard: [] };
  let currentSection: "main" | "sideboard" = "main";
  let seenCards = false;

  for (const raw of lines) {
    const line = raw.trim();

    if (line.startsWith("#")) continue;

    if (line.toLowerCase() === "sideboard") {
      currentSection = "sideboard";
      continue;
    }

    if (line.toLowerCase() === "deck") {
      currentSection = "main";
      continue;
    }

    if (line.toLowerCase() === "companion") {
      continue;
    }

    if (!line) {
      if (seenCards) {
        currentSection = "sideboard";
      }
      continue;
    }

    const match = line.match(/^(\d+)\s+(.+?)\s+\([A-Z0-9]+\)\s+\d+$/);
    if (match) {
      deck[currentSection].push({
        count: parseInt(match[1], 10),
        name: match[2].trim(),
      });
      seenCards = true;
    }
  }

  return deck;
}

/**
 * Auto-detect deck format and parse accordingly.
 * Detects MTGA format by checking for `(SET) NUM` pattern in card lines.
 * Falls back to .dck format parsing.
 */
export function detectAndParseDeck(content: string): ParsedDeck {
  const lines = content.split(/\r?\n/);
  const isMtga = lines.some((line) => {
    const trimmed = line.trim();
    return trimmed && !trimmed.startsWith("#") && MTGA_LINE_PATTERN.test(trimmed);
  });

  return isMtga ? parseMtgaDeck(content) : parseDeckFile(content);
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
