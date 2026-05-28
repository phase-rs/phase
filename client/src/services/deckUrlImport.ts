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
 * Cheap client-side check so the modal can disable the Import button on
 * obviously-wrong input without hitting the network. The worker performs the
 * authoritative validation (and returns 400 unsupported_source for anything
 * that slips through).
 */
export function isSupportedDeckUrl(input: string): boolean {
  try {
    const url = new URL(input.trim());
    const parts = url.pathname.split("/").filter(Boolean);
    if (parts[0] !== "decks" || !parts[1]) return false;
    if (MOXFIELD_HOST_RE.test(url.hostname)) return true;
    if (ARCHIDEKT_HOST_RE.test(url.hostname)) return /^\d+$/.test(parts[1]);
    return false;
  } catch {
    return false;
  }
}

interface ImportError {
  error?: string;
  message?: string;
}

/**
 * Fetch a deck from a Moxfield or Archidekt URL via the deck-import service
 * and return it as canonical decklist text consumable by `detectAndParseDeck`.
 * Throws a user-facing Error for unsupported URLs, network failures, and any
 * upstream error the service surfaces.
 */
export async function fetchDeckFromUrl(input: string): Promise<string> {
  const trimmed = input.trim();
  if (!isSupportedDeckUrl(trimmed)) {
    throw new Error("Enter a valid Moxfield or Archidekt deck URL.");
  }

  const endpoint = `${IMPORT_DECK_BASE}/import-deck?url=${encodeURIComponent(trimmed)}`;
  let resp: Response;
  try {
    resp = await fetch(endpoint);
  } catch {
    throw new Error("Couldn't reach the deck import service. Check your connection and try again.");
  }

  if (!resp.ok) {
    let message = `Import failed (${resp.status}).`;
    try {
      const body = (await resp.json()) as ImportError;
      if (typeof body?.message === "string") message = body.message;
    } catch {
      // Non-JSON error body — keep the generic message.
    }
    throw new Error(message);
  }

  return resp.text();
}
