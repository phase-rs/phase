export interface DeckEntry {
  count: number;
  name: string;
}

export interface ParsedDeck {
  main: DeckEntry[];
  sideboard: DeckEntry[];
  commander?: string[];
}

type DeckSection = "main" | "sideboard" | "commander";
const SIMPLE_DECK_LINE_PATTERN = /^\d+x?\s+.+$/;

function getNamedSection(line: string): DeckSection | null {
  const normalized = line.trim().toLowerCase();
  if (normalized === "deck" || normalized === "[main]") return "main";
  if (normalized === "sideboard" || normalized === "[sideboard]") return "sideboard";
  if (normalized === "commander" || normalized === "[commander]") return "commander";
  return null;
}

function parseDeckEntryLine(line: string): DeckEntry | null {
  const mtgaMatch = line.match(/^(\d+)\s+(.+?)\s+\([A-Z0-9]+\)\s+\d+$/);
  if (mtgaMatch) {
    return {
      count: parseInt(mtgaMatch[1], 10),
      name: mtgaMatch[2].trim(),
    };
  }

  const simpleMatch = line.match(/^(\d+)x?\s+(.+)$/);
  if (simpleMatch) {
    return {
      count: parseInt(simpleMatch[1], 10),
      name: simpleMatch[2].trim(),
    };
  }

  return null;
}

export function deduplicateEntries(entries: DeckEntry[]): DeckEntry[] {
  const map = new Map<string, number>();
  for (const entry of entries) {
    map.set(entry.name, (map.get(entry.name) ?? 0) + entry.count);
  }
  return Array.from(map, ([name, count]) => ({ count, name }));
}

/**
 * Parse a .dck/.dec format deck file.
 * Format: "count CardName" per line (or "countx CardName").
 * Sections: [Main], [Sideboard], [Commander] (case-insensitive).
 * Lines starting with # are comments, empty lines are skipped.
 *
 * Commander auto-detection: cards in [Commander] or [Sideboard] sections
 * of 100-card singleton decks are treated as potential commanders.
 */
export function parseDeckFile(content: string): ParsedDeck {
  const lines = content.split(/\r?\n/);
  const deck: ParsedDeck = { main: [], sideboard: [] };
  const commanderEntries: DeckEntry[] = [];
  let currentSection: DeckSection = "main";

  for (const raw of lines) {
    const line = raw.trim();
    if (!line || line.startsWith("#")) continue;

    const namedSection = getNamedSection(line);
    if (namedSection) {
      currentSection = namedSection;
      continue;
    }

    const entry = parseDeckEntryLine(line);
    if (entry) {
      if (currentSection === "commander") {
        commanderEntries.push(entry);
      } else {
        deck[currentSection].push(entry);
      }
    }
  }

  // If explicit [Commander] section found, extract commander names
  if (commanderEntries.length > 0) {
    deck.commander = commanderEntries.map((e) => e.name);
  }

  deck.main = deduplicateEntries(deck.main);
  deck.sideboard = deduplicateEntries(deck.sideboard);
  return deck;
}

const MTGA_LINE_PATTERN = /^\d+\s+.+\s+\([A-Z0-9]+\)\s+\d+$/;

/**
 * Parse an MTGA text format deck.
 * Format: "count CardName (SET) CollectorNumber" per line.
 * A blank line or "Sideboard" header switches to sideboard section.
 * "Commander" header switches to commander section.
 * Header labels like "Deck", "Companion" are skipped.
 */
export function parseMtgaDeck(content: string): ParsedDeck {
  const lines = content.split(/\r?\n/);
  const deck: ParsedDeck = { main: [], sideboard: [] };
  const commanderEntries: DeckEntry[] = [];
  let currentSection: DeckSection = "main";
  let seenCards = false;

  for (const raw of lines) {
    const line = raw.trim();

    if (line.startsWith("#")) continue;

    const namedSection = getNamedSection(line);
    if (namedSection) {
      currentSection = namedSection;
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

    const entry = parseDeckEntryLine(line);
    if (entry) {
      if (currentSection === "commander") {
        commanderEntries.push(entry);
      } else {
        deck[currentSection].push(entry);
      }
      seenCards = true;
    }
  }

  if (commanderEntries.length > 0) {
    deck.commander = commanderEntries.map((e) => e.name);
  }

  deck.main = deduplicateEntries(deck.main);
  deck.sideboard = deduplicateEntries(deck.sideboard);
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

  const hasNamedSections = lines.some((line) => {
    const trimmed = line.trim();
    return getNamedSection(trimmed) !== null;
  });

  const hasSimpleDeckLines = lines.some((line) => {
    const trimmed = line.trim();
    return trimmed && !trimmed.startsWith("#") && SIMPLE_DECK_LINE_PATTERN.test(trimmed);
  });

  if (isMtga || (hasNamedSections && hasSimpleDeckLines)) {
    return parseMtgaDeck(content);
  }

  return parseDeckFile(content);
}

/**
 * Export a ParsedDeck to .dck format string.
 */
export function exportDeckFile(deck: ParsedDeck): string {
  const lines: string[] = [];

  if (deck.commander && deck.commander.length > 0) {
    lines.push("[Commander]");
    for (const name of deck.commander) {
      lines.push(`1 ${name}`);
    }
  }

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

export type ExportFormat = "dck" | "mtga";

/**
 * Export a ParsedDeck to MTGA text format.
 * Uses simplified format without set/collector number since we don't store that data.
 */
export function exportMtgaDeck(deck: ParsedDeck): string {
  const lines: string[] = [];

  if (deck.commander && deck.commander.length > 0) {
    lines.push("Commander");
    for (const name of deck.commander) {
      lines.push(`1 ${name}`);
    }
    lines.push("");
  }

  lines.push("Deck");
  for (const entry of deck.main) {
    lines.push(`${entry.count} ${entry.name}`);
  }

  if (deck.sideboard.length > 0) {
    lines.push("");
    lines.push("Sideboard");
    for (const entry of deck.sideboard) {
      lines.push(`${entry.count} ${entry.name}`);
    }
  }

  return lines.join("\n") + "\n";
}

/**
 * Export a ParsedDeck in the specified format.
 */
export function exportDeck(deck: ParsedDeck, format: ExportFormat): string {
  return format === "mtga" ? exportMtgaDeck(deck) : exportDeckFile(deck);
}
