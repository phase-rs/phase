export interface DeckEntry {
  count: number;
  name: string;
}

export interface ParsedDeck {
  main: DeckEntry[];
  sideboard: DeckEntry[];
  commander?: string[];
  companion?: string;
}

type DeckSection = "main" | "sideboard" | "commander" | "companion";
const SIMPLE_DECK_LINE_PATTERN = /^\d+x?\s+.+$/;
const BASIC_LANDS = new Set(["Plains", "Island", "Swamp", "Mountain", "Forest"]);

function getNamedSection(line: string): DeckSection | null {
  const normalized = line.trim().toLowerCase();
  if (normalized === "deck" || normalized === "[main]") return "main";
  if (normalized === "sideboard" || normalized === "[sideboard]") return "sideboard";
  if (normalized === "commander" || normalized === "[commander]") return "commander";
  if (normalized === "companion" || normalized === "[companion]") return "companion";
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

function normalizeCardName(name: string): string {
  const trimmed = name.trim();
  if (!trimmed.includes("/")) return trimmed;

  const slashParts = trimmed.split("/").map((part) => part.trim()).filter(Boolean);
  if (slashParts.length === 2) {
    return `${slashParts[0]} // ${slashParts[1]}`;
  }

  return trimmed.replace(/\s*\/{2,}\s*/g, " // ");
}

function normalizeEntries(entries: DeckEntry[]): DeckEntry[] {
  return entries.map((entry) => ({
    ...entry,
    name: normalizeCardName(entry.name),
  }));
}

function totalCards(entries: DeckEntry[]): number {
  return entries.reduce((sum, entry) => sum + entry.count, 0);
}

function looksCommanderSingleton(entries: DeckEntry[]): boolean {
  return entries.every((entry) => entry.count === 1 || BASIC_LANDS.has(entry.name));
}

function normalizeParsedDeck(
  deck: ParsedDeck,
  options: { explicitCommander: boolean; explicitSideboard: boolean },
): ParsedDeck {
  const normalized: ParsedDeck = {
    main: deduplicateEntries(normalizeEntries(deck.main)),
    sideboard: deduplicateEntries(normalizeEntries(deck.sideboard)),
  };

  if (deck.commander?.length) {
    normalized.commander = deck.commander.map(normalizeCardName);
  }

  if (deck.companion) {
    normalized.companion = normalizeCardName(deck.companion);
  }

  if (options.explicitCommander || options.explicitSideboard || normalized.commander?.length) {
    return normalized;
  }

  const mainCount = totalCards(normalized.main);
  const sideboardCount = totalCards(normalized.sideboard);
  if (
    sideboardCount >= 1
    && sideboardCount <= 2
    && mainCount + sideboardCount === 100
    && looksCommanderSingleton(normalized.main)
    && normalized.sideboard.every((entry) => entry.count === 1)
  ) {
    normalized.commander = normalized.sideboard.map((entry) => entry.name);
    normalized.sideboard = [];
  }

  return normalized;
}

export function repairParsedDeck(deck: ParsedDeck): ParsedDeck {
  return normalizeParsedDeck(deck, {
    explicitCommander: false,
    explicitSideboard: false,
  });
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
  let explicitCommander = false;
  let explicitSideboard = false;

  for (const raw of lines) {
    const line = raw.trim();
    if (!line || line.startsWith("#")) continue;

    const namedSection = getNamedSection(line);
    if (namedSection) {
      currentSection = namedSection;
      if (namedSection === "commander") explicitCommander = true;
      if (namedSection === "sideboard") explicitSideboard = true;
      continue;
    }

    const entry = parseDeckEntryLine(line);
    if (entry) {
      if (currentSection === "commander") {
        commanderEntries.push(entry);
      } else if (currentSection === "companion") {
        // CR 702.139a: Record companion name only — the Sideboard section
        // will include the card. loadActiveDeck (storage.ts:98) ensures
        // companion is in sideboard if a source omits it.
        deck.companion = entry.name;
      } else {
        deck[currentSection].push(entry);
      }
    }
  }

  // If explicit [Commander] section found, extract commander names
  if (commanderEntries.length > 0) {
    deck.commander = commanderEntries.map((e) => e.name);
  }

  return normalizeParsedDeck(deck, {
    explicitCommander,
    explicitSideboard,
  });
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
  let explicitCommander = false;
  let explicitSideboard = false;

  for (const raw of lines) {
    const line = raw.trim();

    if (line.startsWith("#")) continue;

    const namedSection = getNamedSection(line);
    if (namedSection) {
      currentSection = namedSection;
      if (namedSection === "commander") explicitCommander = true;
      if (namedSection === "sideboard") explicitSideboard = true;
      continue;
    }

    if (line.toLowerCase() === "companion") {
      currentSection = "companion";
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
      } else if (currentSection === "companion") {
        // CR 702.139a: Record companion name only — the Sideboard section
        // will include the card. loadActiveDeck (storage.ts:98) ensures
        // companion is in sideboard if a source omits it.
        deck.companion = entry.name;
        currentSection = "main"; // Reset after capturing companion
      } else {
        deck[currentSection].push(entry);
      }
      seenCards = true;
    }
  }

  if (commanderEntries.length > 0) {
    deck.commander = commanderEntries.map((e) => e.name);
  }

  return normalizeParsedDeck(deck, {
    explicitCommander,
    explicitSideboard,
  });
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
