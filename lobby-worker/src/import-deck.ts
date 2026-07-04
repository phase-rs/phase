// Deck import service. Given a Moxfield or Archidekt deck URL, fetches the
// upstream API server-side (CORS-free) and returns the canonical decklist text
// the client's deckParser already consumes — same format Paste Text accepts.
//
// The worker is the single authority for source-specific projection: the
// browser client never sees Moxfield's or Archidekt's JSON shape, so adding a
// new source (TappedOut, Deckstats, …) is a server-only change. Responses are
// cached at the edge by full URL so a deck imported many times costs one
// upstream call.

export interface ImportDeckEnv {
  /** Comma-separated origin allowlist, or "*" (default) to allow any. */
  ALLOWED_ORIGINS?: string;
}

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

type ErrorCode =
  | "invalid_url"
  | "method_not_allowed"
  | "unsupported_source"
  | "invalid_id"
  | "not_found"
  | "upstream_unavailable"
  | "upstream_timeout";

const MOXFIELD_HOSTS = new Set(["moxfield.com", "www.moxfield.com"]);
const ARCHIDEKT_HOSTS = new Set(["archidekt.com", "www.archidekt.com"]);
const CACHE_TTL_SECONDS = 900; // 15 min — long enough to amortize hot decks,
                               // short enough that an updated deck re-syncs.
const UPSTREAM_TIMEOUT_MS = 8000;
const USER_AGENT = "phase-rs-deck-importer (+https://phase-rs.dev)";

function corsHeaders(request: Request, env: ImportDeckEnv): Record<string, string> {
  const allow = (env.ALLOWED_ORIGINS ?? "*").trim();
  let allowOrigin = "*";
  if (allow !== "*") {
    const origin = request.headers.get("Origin") ?? "";
    const list = allow.split(",").map((s) => s.trim()).filter(Boolean);
    allowOrigin = list.includes(origin) ? origin : (list[0] ?? "");
  }
  return {
    "Access-Control-Allow-Origin": allowOrigin,
    "Access-Control-Allow-Methods": "GET, OPTIONS",
    "Access-Control-Allow-Headers": "Content-Type",
    Vary: "Origin",
  };
}

function errorResponse(
  status: number,
  code: ErrorCode,
  message: string,
  cors: Record<string, string>,
  extra: Record<string, unknown> = {},
): Response {
  return Response.json({ error: code, message, ...extra }, { status, headers: cors });
}

// ---------------------------------------------------------------------------
// URL parsing
// ---------------------------------------------------------------------------

type Source = "moxfield" | "archidekt";

interface ParsedDeckUrl {
  source: Source;
  id: string;
}

function parseDeckUrl(raw: string): ParsedDeckUrl | null {
  // Direct API consumers (curl, third-party clients) often omit the protocol.
  // The WHATWG URL parser requires one; normalize so we accept both forms.
  const trimmed = raw
    .trim()
    .replace(/[)\].,!?:;]+$/u, "")
    .replace(/^<(.+)>$/, "$1")
    .replace(/[)\].,!?:;]+$/u, "");
  const withScheme = /^https?:\/\//i.test(trimmed) ? trimmed : `https://${trimmed}`;
  let url: URL;
  try {
    url = new URL(withScheme);
  } catch {
    return null;
  }
  const parts = url.pathname.split("/").filter(Boolean);
  if (parts[0] !== "decks" || !parts[1]) return null;

  if (MOXFIELD_HOSTS.has(url.hostname)) {
    return { source: "moxfield", id: parts[1] };
  }
  if (ARCHIDEKT_HOSTS.has(url.hostname) && /^\d+$/.test(parts[1])) {
    return { source: "archidekt", id: parts[1] };
  }
  return null;
}

// ---------------------------------------------------------------------------
// Canonical decklist text builder
// ---------------------------------------------------------------------------

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
// Each board maps <id> -> { quantity, card: { name, set, cn } }. The v2 payload
// nests boards under a top-level `boards` map, each board exposing its entries
// under `.cards`; older/simplified shapes expose the board map at the top level.
// `moxfieldBoard` reads whichever is present so an import survives either shape.
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

interface MoxfieldBoard {
  cards?: Record<string, MoxfieldEntry>;
}

interface MoxfieldDeck {
  name?: unknown;
  boards?: Record<string, MoxfieldBoard>;
  mainboard?: Record<string, MoxfieldEntry>;
  sideboard?: Record<string, MoxfieldEntry>;
  commanders?: Record<string, MoxfieldEntry>;
  companions?: Record<string, MoxfieldEntry>;
}

// Resolve a board's entry map by key, accepting both the nested v2 shape
// (`boards.<key>.cards`) and the flat top-level shape (`deck.<key>`).
function moxfieldBoard(
  deck: MoxfieldDeck,
  key: "mainboard" | "sideboard" | "commanders" | "companions",
): Record<string, MoxfieldEntry> | undefined {
  const nested = deck.boards?.[key]?.cards;
  if (nested && typeof nested === "object") return nested;
  const top = deck[key];
  return top && typeof top === "object" ? top : undefined;
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

// An import is only "empty" when no board produced any card. Guarding on just
// main+commander wrongly rejected sideboard-only or companion-only imports.
function sectionsEmpty(sections: DeckSections): boolean {
  return (
    sections.commander.length === 0 &&
    sections.main.length === 0 &&
    sections.sideboard.length === 0 &&
    sections.companion.length === 0
  );
}

function projectMoxfield(deck: MoxfieldDeck): { text: string; empty: boolean } {
  // Only commander/main/sideboard/companion boards are imported. Moxfield's
  // `maybeboard` (and Archidekt's Maybeboard category) are test-deck slots —
  // see classifyArchidektCard for Archidekt exclusion.
  const sections: DeckSections = {
    commander: moxfieldBoardToCards(moxfieldBoard(deck, "commanders")),
    main: moxfieldBoardToCards(moxfieldBoard(deck, "mainboard")),
    sideboard: moxfieldBoardToCards(moxfieldBoard(deck, "sideboard")),
    companion: moxfieldBoardToCards(moxfieldBoard(deck, "companions")),
  };
  if (sectionsEmpty(sections)) {
    return { text: "", empty: true };
  }
  return { text: buildDeckText(asString(deck.name), sections), empty: false };
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

function projectArchidekt(deck: ArchidektDeck): { text: string; empty: boolean } {
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

  if (sectionsEmpty(sections)) {
    return { text: "", empty: true };
  }
  return { text: buildDeckText(asString(deck.name), sections), empty: false };
}

// ---------------------------------------------------------------------------
// Upstream fetch (with timeout)
// ---------------------------------------------------------------------------

type UpstreamResult =
  | { ok: true; json: unknown }
  | { ok: false; status: number }; // status 0 = network/timeout, -1 = bad JSON

async function fetchUpstream(url: string): Promise<UpstreamResult> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), UPSTREAM_TIMEOUT_MS);
  let resp: Response;
  try {
    resp = await fetch(url, {
      headers: { "User-Agent": USER_AGENT, Accept: "application/json" },
      signal: controller.signal,
    });
  } catch {
    return { ok: false, status: 0 }; // network failure or AbortController timeout
  } finally {
    clearTimeout(timer);
  }
  if (!resp.ok) return { ok: false, status: resp.status };
  try {
    return { ok: true, json: await resp.json() };
  } catch {
    // Upstream returned a 2xx body that isn't JSON — maintenance page, HTML
    // login wall, edge cache miss, etc. Surface as a distinct error so the
    // caller doesn't conflate it with a network timeout (504 → 502).
    return { ok: false, status: -1 };
  }
}

const SOURCE_ENDPOINTS: Record<Source, (id: string) => string> = {
  moxfield: (id) => `https://api2.moxfield.com/v2/decks/all/${encodeURIComponent(id)}`,
  archidekt: (id) => `https://archidekt.com/api/decks/${encodeURIComponent(id)}/`,
};

function project(source: Source, json: unknown): { text: string; empty: boolean } {
  if (source === "moxfield") return projectMoxfield(json as MoxfieldDeck);
  return projectArchidekt(json as ArchidektDeck);
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

export async function handleImportDeck(
  request: Request,
  env: ImportDeckEnv,
  ctx?: Pick<ExecutionContext, "waitUntil">,
): Promise<Response> {
  const cors = corsHeaders(request, env);
  if (request.method === "OPTIONS") {
    return new Response(null, { status: 204, headers: cors });
  }
  if (request.method !== "GET") {
    return errorResponse(405, "method_not_allowed", "Method not allowed.", cors);
  }

  const requestUrl = new URL(request.url);
  const rawDeckUrl = requestUrl.searchParams.get("url");
  if (!rawDeckUrl) {
    return errorResponse(400, "invalid_url", "Missing `url` query parameter.", cors);
  }

  const parsed = parseDeckUrl(rawDeckUrl);
  if (!parsed) {
    return errorResponse(
      400,
      "unsupported_source",
      "Unsupported link. Paste a Moxfield (moxfield.com/decks/…) or Archidekt (archidekt.com/decks/…) deck URL.",
      cors,
    );
  }

  // CF Cache lookup — key on the full inbound URL so the query string is part
  // of the key (caches.default ignores headers we didn't set Vary on).
  const cache = (globalThis as { caches?: CacheStorage }).caches?.default;
  const cacheKey = new Request(request.url, { method: "GET" });
  if (cache) {
    const hit = await cache.match(cacheKey);
    if (hit) {
      // Copy so we can add CORS for the requesting origin (cached response was
      // stored with whoever's CORS first warmed it).
      const headers = new Headers(hit.headers);
      for (const [k, v] of Object.entries(cors)) headers.set(k, v);
      return new Response(hit.body, { status: hit.status, headers });
    }
  }

  const upstreamUrl = SOURCE_ENDPOINTS[parsed.source](parsed.id);
  const upstream = await fetchUpstream(upstreamUrl);
  if (!upstream.ok) {
    if (upstream.status === 404) {
      return errorResponse(
        404,
        "not_found",
        `${parsed.source === "moxfield" ? "Moxfield" : "Archidekt"} deck not found or private.`,
        cors,
        { source: parsed.source },
      );
    }
    if (upstream.status === 0) {
      console.error({ event: "import_deck_upstream_timeout", source: parsed.source, id: parsed.id });
      return errorResponse(
        504,
        "upstream_timeout",
        `${parsed.source === "moxfield" ? "Moxfield" : "Archidekt"} didn't respond in time. Try again in a moment.`,
        cors,
        { source: parsed.source },
      );
    }
    if (upstream.status === -1) {
      console.error({ event: "import_deck_upstream_bad_json", source: parsed.source, id: parsed.id });
      return errorResponse(
        502,
        "upstream_unavailable",
        `${parsed.source === "moxfield" ? "Moxfield" : "Archidekt"} returned an unexpected response. The deck source may be down or behind a maintenance page.`,
        cors,
        { source: parsed.source },
      );
    }
    console.error({
      event: "import_deck_upstream_error",
      source: parsed.source,
      id: parsed.id,
      upstreamStatus: upstream.status,
    });
    return errorResponse(
      502,
      "upstream_unavailable",
      `${parsed.source === "moxfield" ? "Moxfield" : "Archidekt"} returned ${upstream.status}. The deck may be private or removed.`,
      cors,
      { source: parsed.source },
    );
  }

  const { text, empty } = project(parsed.source, upstream.json);
  if (empty) {
    return errorResponse(
      404,
      "not_found",
      `${parsed.source === "moxfield" ? "Moxfield" : "Archidekt"} deck has no cards, or it is private.`,
      cors,
      { source: parsed.source },
    );
  }

  const response = new Response(text, {
    status: 200,
    headers: {
      ...cors,
      "Content-Type": "text/plain; charset=utf-8",
      "Cache-Control": `public, max-age=${CACHE_TTL_SECONDS}`,
    },
  });

  if (cache && ctx) {
    // Cache *clone* — writing to the cache consumes the body, and we still
    // need to return it.
    ctx.waitUntil(cache.put(cacheKey, response.clone()));
  }

  console.log({ event: "import_deck_ok", source: parsed.source, id: parsed.id, bytes: text.length });
  return response;
}
