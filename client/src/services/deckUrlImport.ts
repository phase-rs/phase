// Fetches decks from Moxfield and Archidekt deck URLs and projects them onto
// the canonical decklist text the deckParser already understands (Name header
// + [Commander]/[Main]/[Sideboard]/[Companion] sections, MTGA-style printing
// suffixes). All parsing/normalization stays in deckParser — this module is a
// thin source adapter, mirroring fetchCubeList in cubeCobra.ts.

const MOXFIELD_HOSTS = new Set(["moxfield.com", "www.moxfield.com"]);
const ARCHIDEKT_HOSTS = new Set(["archidekt.com", "www.archidekt.com"]);

interface ImportCard {
  count: number;
  name: string;
  set?: string;
  collectorNumber?: string;
}

interface DeckSections {
  commander: ImportCard[];
  main: ImportCard[];
  sideboard: ImportCard[];
  companion: ImportCard[];
}

// A bare set code (no spaces, alphanumeric) is required for the MTGA printing
// suffix to round-trip through deckParser's `(SET) collector` matcher.
function cardLine(card: ImportCard): string {
  const set = card.set?.trim();
  const cn = card.collectorNumber?.trim();
  if (set && cn && /^[A-Za-z0-9]+$/.test(set) && !/\s/.test(cn)) {
    return `${card.count} ${card.name} (${set.toUpperCase()}) ${cn}`;
  }
  return `${card.count} ${card.name}`;
}

function pushSection(lines: string[], header: string, cards: ImportCard[]): void {
  if (cards.length === 0) return;
  lines.push(header);
  for (const card of cards) lines.push(cardLine(card));
}

// Companion is emitted last: parseMtgaDeck collapses the companion section back
// to "main" after the first card, so any cards following it would be misfiled.
function buildDeckText(name: string | undefined, sections: DeckSections): string {
  const lines: string[] = [];
  if (name?.trim()) lines.push(`Name: ${name.trim()}`);
  pushSection(lines, "[Commander]", sections.commander);
  pushSection(lines, "[Main]", sections.main);
  pushSection(lines, "[Sideboard]", sections.sideboard);
  pushSection(lines, "[Companion]", sections.companion);
  return lines.join("\n") + "\n";
}

function asString(value: unknown): string | undefined {
  if (typeof value === "string") return value;
  if (typeof value === "number") return String(value);
  return undefined;
}

// ---------------------------------------------------------------------------
// Moxfield — https://api2.moxfield.com/v2/decks/all/<publicId>
// Boards are top-level maps of <id> -> { quantity, card: { name, set, cn } }.
// ---------------------------------------------------------------------------

interface MoxfieldCard {
  name?: unknown;
  set?: unknown;
  cn?: unknown;
}

interface MoxfieldEntry {
  quantity?: unknown;
  card?: MoxfieldCard;
}

interface MoxfieldDeck {
  name?: unknown;
  mainboard?: Record<string, MoxfieldEntry>;
  sideboard?: Record<string, MoxfieldEntry>;
  commanders?: Record<string, MoxfieldEntry>;
  companions?: Record<string, MoxfieldEntry>;
}

function moxfieldBoardToCards(board: Record<string, MoxfieldEntry> | undefined): ImportCard[] {
  if (!board || typeof board !== "object") return [];
  const cards: ImportCard[] = [];
  for (const entry of Object.values(board)) {
    const name = asString(entry?.card?.name)?.trim();
    const count = typeof entry?.quantity === "number" ? entry.quantity : 0;
    if (!name || count <= 0) continue;
    cards.push({
      count,
      name,
      set: asString(entry?.card?.set),
      collectorNumber: asString(entry?.card?.cn),
    });
  }
  return cards;
}

function moxfieldDeckToText(deck: MoxfieldDeck): string {
  const sections: DeckSections = {
    commander: moxfieldBoardToCards(deck.commanders),
    main: moxfieldBoardToCards(deck.mainboard),
    sideboard: moxfieldBoardToCards(deck.sideboard),
    companion: moxfieldBoardToCards(deck.companions),
  };
  if (sections.main.length === 0 && sections.commander.length === 0) {
    throw new Error("Moxfield deck has no cards, or it is private.");
  }
  return buildDeckText(asString(deck.name), sections);
}

async function fetchMoxfieldDeck(id: string): Promise<string> {
  let resp: Response;
  try {
    resp = await fetch(`https://api2.moxfield.com/v2/decks/all/${encodeURIComponent(id)}`);
  } catch {
    throw new Error(
      "Couldn't reach Moxfield. Their API blocks some browser requests — "
        + "export the deck and use Paste Text instead.",
    );
  }
  if (!resp.ok) {
    throw new Error(`Moxfield request failed (${resp.status}). The deck may be private or removed.`);
  }
  return moxfieldDeckToText((await resp.json()) as MoxfieldDeck);
}

// ---------------------------------------------------------------------------
// Archidekt — https://archidekt.com/api/decks/<id>/
// `cards` is a flat array; each entry carries its category names, and the deck
// `categories` array marks which categories are included in the deck.
// ---------------------------------------------------------------------------

interface ArchidektCardEntry {
  quantity?: unknown;
  categories?: unknown;
  card?: {
    oracleCard?: { name?: unknown };
    edition?: { editioncode?: unknown };
    collectorNumber?: unknown;
  };
}

interface ArchidektCategory {
  name?: unknown;
  includedInDeck?: unknown;
}

interface ArchidektDeck {
  name?: unknown;
  cards?: unknown;
  categories?: unknown;
}

type Bucket = keyof DeckSections | "skip";

function archidektCategoryInclusion(raw: unknown): Map<string, boolean> {
  const map = new Map<string, boolean>();
  if (!Array.isArray(raw)) return map;
  for (const category of raw as ArchidektCategory[]) {
    const name = asString(category?.name)?.trim().toLowerCase();
    if (!name) continue;
    map.set(name, category?.includedInDeck !== false);
  }
  return map;
}

function classifyArchidektCard(categories: string[], inclusion: Map<string, boolean>): Bucket {
  for (const raw of categories) {
    const name = raw.trim().toLowerCase();
    if (name === "commander" || name === "commanders") return "commander";
    if (name === "companion") return "companion";
    if (name === "sideboard") return "sideboard";
    if (name === "maybeboard") return "skip";
  }
  // Cards whose only categories are excluded from the deck (custom maybeboards)
  // should not enter the main deck.
  if (categories.length > 0 && categories.every((c) => inclusion.get(c.trim().toLowerCase()) === false)) {
    return "skip";
  }
  return "main";
}

function archidektDeckToText(deck: ArchidektDeck): string {
  const entries = Array.isArray(deck.cards) ? (deck.cards as ArchidektCardEntry[]) : [];
  const inclusion = archidektCategoryInclusion(deck.categories);
  const sections: DeckSections = { commander: [], main: [], sideboard: [], companion: [] };

  for (const entry of entries) {
    const name = asString(entry?.card?.oracleCard?.name)?.trim();
    const count = typeof entry?.quantity === "number" ? entry.quantity : 0;
    if (!name || count <= 0) continue;

    const card: ImportCard = {
      count,
      name,
      set: asString(entry?.card?.edition?.editioncode),
      collectorNumber: asString(entry?.card?.collectorNumber),
    };

    const categories = Array.isArray(entry?.categories)
      ? (entry.categories as unknown[]).filter((c): c is string => typeof c === "string")
      : [];
    const bucket = classifyArchidektCard(categories, inclusion);
    if (bucket === "skip") continue;
    sections[bucket].push(card);
  }

  if (sections.main.length === 0 && sections.commander.length === 0) {
    throw new Error("Archidekt deck has no cards, or it is private.");
  }
  return buildDeckText(asString(deck.name), sections);
}

async function fetchArchidektDeck(id: string): Promise<string> {
  let resp: Response;
  try {
    resp = await fetch(`https://archidekt.com/api/decks/${encodeURIComponent(id)}/`);
  } catch {
    throw new Error("Couldn't reach Archidekt. Check the URL and your connection.");
  }
  if (!resp.ok) {
    throw new Error(`Archidekt request failed (${resp.status}). The deck may be private or removed.`);
  }
  return archidektDeckToText((await resp.json()) as ArchidektDeck);
}

// ---------------------------------------------------------------------------

function moxfieldDeckId(url: URL): string | null {
  if (!MOXFIELD_HOSTS.has(url.hostname)) return null;
  const parts = url.pathname.split("/").filter(Boolean);
  return parts[0] === "decks" && parts[1] ? parts[1] : null;
}

function archidektDeckId(url: URL): string | null {
  if (!ARCHIDEKT_HOSTS.has(url.hostname)) return null;
  const parts = url.pathname.split("/").filter(Boolean);
  return parts[0] === "decks" && /^\d+$/.test(parts[1] ?? "") ? parts[1] : null;
}

export function isSupportedDeckUrl(input: string): boolean {
  try {
    const url = new URL(input.trim());
    return moxfieldDeckId(url) !== null || archidektDeckId(url) !== null;
  } catch {
    return false;
  }
}

/**
 * Fetch a deck from a Moxfield or Archidekt URL and return it as canonical
 * decklist text consumable by `detectAndParseDeck`. Throws a user-facing
 * Error for unsupported URLs, network/CORS failures, and private decks.
 */
export async function fetchDeckFromUrl(input: string): Promise<string> {
  let url: URL;
  try {
    url = new URL(input.trim());
  } catch {
    throw new Error("Enter a valid Moxfield or Archidekt deck URL.");
  }

  const moxId = moxfieldDeckId(url);
  if (moxId) return fetchMoxfieldDeck(moxId);

  const archId = archidektDeckId(url);
  if (archId) return fetchArchidektDeck(archId);

  throw new Error(
    "Unsupported link. Paste a Moxfield (moxfield.com/decks/…) "
      + "or Archidekt (archidekt.com/decks/…) deck URL.",
  );
}
