import { isCardCommanderEligible } from "./engineRuntime";
import { BASIC_LAND_NAMES } from "../constants/game";

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

/**
 * Flat deck shape consumed by the engine (`PlayerDeckList` in Rust) and by
 * `evaluate_deck_compatibility_js`. The single authority for projecting a
 * `ParsedDeck` to this shape is `expandParsedDeck` below — all callers MUST
 * use it so the commander slot (and any future field) cannot be dropped on
 * one code path while preserved on another.
 */
export interface ExpandedDeck {
  main_deck: string[];
  sideboard: string[];
  commander: string[];
}

function expandEntries(entries: DeckEntry[]): string[] {
  const cards: string[] = [];
  for (const entry of entries) {
    for (let i = 0; i < entry.count; i++) {
      cards.push(entry.name);
    }
  }
  return cards;
}

/**
 * Project a `ParsedDeck` to the flat `{ main_deck, sideboard, commander }`
 * shape the engine validates and loads. This is the only supported bridge
 * between the client's structured deck model and the engine payload — used
 * by both the upfront format-legality check and the host adapter's
 * `initializeGame` call so their inputs can never diverge.
 */
export function expandParsedDeck(deck: ParsedDeck): ExpandedDeck {
  return {
    main_deck: expandEntries(deck.main),
    sideboard: expandEntries(deck.sideboard),
    commander: deck.commander ?? [],
  };
}

type DeckSection = "main" | "sideboard" | "commander" | "companion";
const SIMPLE_DECK_LINE_PATTERN = /^\d+x?\s+.+$/;

// Archidekt / Moxfield category annotations on an individual card line:
//   "1 Zimone (SOC) 10 [Commander {top}]"  /  "1x Zimone *CMDR*"  /  "[Companion]"
const COMMANDER_ANNOTATION_RE = /\s*(?:\[Commander(?:\s*\{[^}]*\})?\]|\*CMDR\*|\*Commander\*)\s*$/i;
const COMPANION_ANNOTATION_RE = /\s*(?:\[Companion(?:\s*\{[^}]*\})?\]|\*Companion\*)\s*$/i;

// "Commanders" is the section label Archidekt uses when exporting with categories.
function getNamedSection(line: string): DeckSection | null {
  const normalized = line.trim().toLowerCase();
  if (normalized === "deck" || normalized === "[main]" || normalized === "mainboard") return "main";
  if (normalized === "sideboard" || normalized === "[sideboard]") return "sideboard";
  if (
    normalized === "commander"
    || normalized === "commanders"
    || normalized === "[commander]"
    || normalized === "[commanders]"
  ) return "commander";
  if (normalized === "companion" || normalized === "[companion]") return "companion";
  return null;
}

interface LineParseResult {
  entry: DeckEntry;
  annotation: "commander" | "companion" | null;
}

// MTGA set-code parens are now permissive: empty `()` appears in some exports
// (e.g. Archidekt's "Three Visits () 315" when the printing has no set code).
function parseDeckEntryLine(line: string): LineParseResult | null {
  let remainder = line;
  let annotation: LineParseResult["annotation"] = null;
  if (COMMANDER_ANNOTATION_RE.test(remainder)) {
    remainder = remainder.replace(COMMANDER_ANNOTATION_RE, "");
    annotation = "commander";
  } else if (COMPANION_ANNOTATION_RE.test(remainder)) {
    remainder = remainder.replace(COMPANION_ANNOTATION_RE, "");
    annotation = "companion";
  }

  const mtgaMatch = remainder.match(/^(\d+)\s+(.+?)\s+\([A-Z0-9]*\)\s+\S+$/);
  if (mtgaMatch) {
    return {
      entry: { count: parseInt(mtgaMatch[1], 10), name: mtgaMatch[2].trim() },
      annotation,
    };
  }

  const simpleMatch = remainder.match(/^(\d+)x?\s+(.+)$/);
  if (simpleMatch) {
    return {
      entry: { count: parseInt(simpleMatch[1], 10), name: simpleMatch[2].trim() },
      annotation,
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
  return entries.every((entry) => entry.count === 1 || BASIC_LAND_NAMES.has(entry.name));
}

function removeOneCopy(entries: DeckEntry[], name: string): DeckEntry[] {
  let removed = false;
  const next: DeckEntry[] = [];

  for (const entry of entries) {
    if (!removed && entry.name.toLowerCase() === name.toLowerCase()) {
      removed = true;
      if (entry.count > 1) {
        next.push({ ...entry, count: entry.count - 1 });
      }
      continue;
    }
    next.push(entry);
  }

  return next;
}

function removeCommandersFromMain(deck: ParsedDeck): ParsedDeck {
  if (!deck.commander?.length) return deck;

  let main = deck.main;
  for (const commander of deck.commander) {
    main = removeOneCopy(main, commander);
  }

  return { ...deck, main };
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

  const canonical = removeCommandersFromMain(normalized);

  if (options.explicitCommander || options.explicitSideboard || canonical.commander?.length) {
    return canonical;
  }

  const mainCount = totalCards(canonical.main);
  const sideboardCount = totalCards(canonical.sideboard);
  if (
    sideboardCount >= 1
    && sideboardCount <= 2
    && mainCount + sideboardCount === 100
    && looksCommanderSingleton(canonical.main)
    && canonical.sideboard.every((entry) => entry.count === 1)
  ) {
    canonical.commander = canonical.sideboard.map((entry) => entry.name);
    canonical.sideboard = [];
  }

  return removeCommandersFromMain(canonical);
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

    const parsed = parseDeckEntryLine(line);
    if (parsed) {
      const { entry, annotation } = parsed;
      if (annotation === "commander" || currentSection === "commander") {
        commanderEntries.push(entry);
        if (annotation === "commander") explicitCommander = true;
      } else if (annotation === "companion" || currentSection === "companion") {
        // CR 702.139a: Record companion name only — the Sideboard section
        // will include the card. loadActiveDeck (storage.ts:98) ensures
        // companion is in sideboard if a source omits it.
        deck.companion = entry.name;
      } else {
        deck[currentSection].push(entry);
      }
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

// MTGA format detection: count + name + (set) + collector#, with optional
// trailing Archidekt category annotation (e.g. "[Commander {top}]").
const MTGA_LINE_PATTERN = /^\d+\s+.+\s+\([A-Z0-9]*\)\s+\S+(\s+\S.*)?$/;

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

    const parsed = parseDeckEntryLine(line);
    if (parsed) {
      const { entry, annotation } = parsed;
      if (annotation === "commander" || currentSection === "commander") {
        commanderEntries.push(entry);
        if (annotation === "commander") explicitCommander = true;
      } else if (annotation === "companion" || currentSection === "companion") {
        // CR 702.139a: Record companion name only — the Sideboard section
        // will include the card. loadActiveDeck (storage.ts:98) ensures
        // companion is in sideboard if a source omits it.
        deck.companion = entry.name;
        if (currentSection === "companion") currentSection = "main";
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
