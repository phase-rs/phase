// Thin client for the phase.rs deck-import service. The worker
// (lobby-worker/src/import-deck.ts) owns all source-specific projection
// (Moxfield, Archidekt, future sources) — the browser only knows about phase's
// own decklist text format, which deckParser already consumes. Going through
// the worker also sidesteps CORS, which both upstreams enforce on browsers.

// Default points at the official lobby worker in production builds; the dev
// build uses a relative path so Vite's proxy can forward to a local
// `wrangler dev` instance without CORS. Override with VITE_IMPORT_DECK_URL
// when self-hosting.
const IMPORT_DECK_BASE =
  import.meta.env.VITE_IMPORT_DECK_URL
  ?? (import.meta.env.DEV ? "" : "https://lobby.phase-rs.dev");

const MOXFIELD_HOST_RE = /^(?:www\.)?moxfield\.com$/;
const ARCHIDEKT_HOST_RE = /^(?:www\.)?archidekt\.com$/;

/**
 * Translation keys for the two frontend-authored errors this module can throw.
 * Distinct from upstream worker error messages, which arrive as JSON bodies
 * and flow through verbatim as `Error.message` (server-authored pass-through,
 * per `client/src/i18n/README.md`). The modal layer uses the `importDeck.`
 * prefix to detect that a thrown message is a translation key vs server text.
 */
export const IMPORT_ERROR_KEYS = {
  invalidUrl: "importDeck.errorInvalidUrl",
  networkFailure: "importDeck.errorNetworkFailure",
} as const;

// Users commonly paste URLs without the protocol ("moxfield.com/decks/abc").
// The WHATWG URL parser requires a scheme, so normalize once at the boundary
// and let downstream validators see a uniform shape.
function normalizeDeckUrl(input: string): string {
  const trimmed = input
    .trim()
    .replace(/[)\].,!?:;]+$/u, "")
    .replace(/^<(.+)>$/, "$1")
    .replace(/[)\].,!?:;]+$/u, "");
  return /^https?:\/\//i.test(trimmed) ? trimmed : `https://${trimmed}`;
}

/**
 * Cheap client-side check so the modal can disable the Import button on
 * obviously-wrong input without hitting the network. The worker performs the
 * authoritative validation (and returns 400 unsupported_source for anything
 * that slips through).
 */
export function isSupportedDeckUrl(input: string): boolean {
  try {
    const url = new URL(normalizeDeckUrl(input));
    const parts = url.pathname.split("/").filter(Boolean);
    if (parts[0] !== "decks" || !parts[1]) return false;
    if (MOXFIELD_HOST_RE.test(url.hostname)) return true;
    if (ARCHIDEKT_HOST_RE.test(url.hostname)) return /^\d+$/.test(parts[1]);
    return false;
  } catch {
    return false;
  }
}

interface ImportErrorBody {
  error?: string;
  message?: string;
}

/**
 * Fetch a deck from a Moxfield or Archidekt URL via the deck-import service
 * and return it as canonical decklist text consumable by `detectAndParseDeck`.
 *
 * Throws `Error` with a `.message` that is either:
 *   - one of `IMPORT_ERROR_KEYS` (translation key, frontend-authored), or
 *   - a worker-authored message (server pass-through, displayed verbatim).
 */
export async function fetchDeckFromUrl(input: string): Promise<string> {
  const normalized = normalizeDeckUrl(input);
  if (!isSupportedDeckUrl(normalized)) {
    throw new Error(IMPORT_ERROR_KEYS.invalidUrl);
  }

  const endpoint = `${IMPORT_DECK_BASE}/import-deck?url=${encodeURIComponent(normalized)}`;
  let resp: Response;
  try {
    resp = await fetch(endpoint);
  } catch {
    throw new Error(IMPORT_ERROR_KEYS.networkFailure);
  }

  if (!resp.ok) {
    let message = `Import failed (${resp.status}).`;
    try {
      const body = (await resp.json()) as ImportErrorBody;
      if (typeof body?.message === "string") message = body.message;
    } catch {
      // Non-JSON error body — keep the generic message.
    }
    throw new Error(message);
  }

  return resp.text();
}
