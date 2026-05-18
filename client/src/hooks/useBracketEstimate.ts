import { useEffect, useRef, useState } from "react";

import { AdapterError, AdapterErrorCode } from "../adapter/types";
import type { EngineAdapter, GameFormat } from "../adapter/types";
import type { BracketEstimate } from "../types/bracket";
import { isCommanderFamilyFormat } from "../types/bracket";
import type { ParsedDeck } from "../services/deckParser";

const DEBOUNCE_MS = 200;

/**
 * Cross-instance cache: deck-key → most-recent promise. Lets multiple
 * hook instances (e.g. one per row on MyDecksPage) share results for
 * identical decks without each re-hitting the WASM bridge.
 *
 * Lives at module scope; survives component unmount. The data_version
 * stamped onto each BracketEstimate handles bracket_lists.json updates
 * naturally — a hot-reload of card data will produce new keys.
 */
const cache = new Map<string, Promise<BracketEstimate | null>>();
const CACHE_MAX_ENTRIES = 256;

function readCacheOrFetch(
  deckKey: string,
  fetcher: () => Promise<BracketEstimate | null>,
): Promise<BracketEstimate | null> {
  const cached = cache.get(deckKey);
  if (cached) return cached;
  const promise = fetcher();
  cache.set(deckKey, promise);
  // simple LRU cap
  if (cache.size > CACHE_MAX_ENTRIES) {
    const firstKey = cache.keys().next().value;
    if (firstKey !== undefined) cache.delete(firstKey);
  }
  return promise;
}

/** Clears the module-level bracket estimate cache. Use in test afterEach hooks. */
export function clearBracketEstimateCache(): void {
  cache.clear();
}

interface Options {
  deck: ParsedDeck;
  commanders: string[];
  format: GameFormat | undefined;
  adapter: Pick<EngineAdapter, "estimateBracket">;
}

interface Result {
  estimate: BracketEstimate | null;
  loading: boolean;
  /**
   * True when the active adapter (Tauri, WebSocket, P2P, server-draft) threw
   * `BRACKET_ESTIMATION_UNSUPPORTED`. Lets callers distinguish "this build
   * doesn't support local bracket estimation" from "deck has no commander".
   */
  unsupported: boolean;
}

/**
 * Live, debounced bracket estimate for the current deck. Returns
 * `{ estimate: null, loading: false }` when the deck is not a Commander
 * deck or no commander is selected — the audit panel uses these flags
 * to decide whether to render the empty-state placeholder.
 *
 * Debounced 200ms. Memoized by deck contents so re-renders with identical
 * inputs don't refire. A `pendingKeyRef` written synchronously at schedule
 * time guards against stale async results: if a newer effect supersedes
 * before the in-flight one resolves, the stale resolution is discarded.
 */
export function useBracketEstimate({
  deck,
  commanders,
  format,
  adapter,
}: Options): Result {
  const [estimate, setEstimate] = useState<BracketEstimate | null>(null);
  const [loading, setLoading] = useState(false);
  const [unsupported, setUnsupported] = useState(false);
  /** Last successfully *stored* key — used to short-circuit identical re-renders. */
  const storedKeyRef = useRef<string | null>(null);
  /** Latest *scheduled* key — written synchronously, used as the stale-result guard. */
  const pendingKeyRef = useRef<string | null>(null);

  const eligible = isCommanderFamilyFormat(format) && commanders.length > 0;

  const deckKey = (() => {
    if (!eligible) return null;
    const parts: string[] = [...commanders.map((c) => `c:${c.toLowerCase()}`)];
    for (const e of deck.main) parts.push(`m:${e.count}x${e.name.toLowerCase()}`);
    parts.sort();
    return parts.join("|");
  })();

  useEffect(() => {
    if (!eligible || !deckKey) {
      setEstimate(null);
      setLoading(false);
      setUnsupported(false);
      storedKeyRef.current = null;
      pendingKeyRef.current = null;
      return;
    }
    if (deckKey === storedKeyRef.current) return;

    pendingKeyRef.current = deckKey;
    setLoading(true);
    setUnsupported(false);
    const scheduledKey = deckKey;
    const timer = setTimeout(async () => {
      try {
        const result = await readCacheOrFetch(scheduledKey, () =>
          adapter.estimateBracket({
            commander: commanders,
            main_deck: deck.main.flatMap((e) => Array(e.count).fill(e.name)),
            sideboard: deck.sideboard.flatMap((e) => Array(e.count).fill(e.name)),
          }),
        );
        if (pendingKeyRef.current !== scheduledKey) {
          // A newer effect superseded us; discard this stale result.
          return;
        }
        storedKeyRef.current = scheduledKey;
        setEstimate(result);
        setUnsupported(false);
      } catch (err) {
        if (pendingKeyRef.current !== scheduledKey) return;
        setEstimate(null);
        setUnsupported(
          err instanceof AdapterError &&
            err.code === AdapterErrorCode.BRACKET_ESTIMATION_UNSUPPORTED,
        );
      } finally {
        if (pendingKeyRef.current === scheduledKey) {
          setLoading(false);
        }
      }
    }, DEBOUNCE_MS);
    return () => clearTimeout(timer);
    // `commanders`, `deck.main`, and `deck.sideboard` are intentionally
    // omitted: their content is fully captured by `deckKey`, which changes
    // only when the deck actually differs. Including the raw arrays would
    // cause re-runs on every object-identity churn with no observable change.
  }, [eligible, deckKey, adapter]); // eslint-disable-line react-hooks/exhaustive-deps

  return { estimate, loading, unsupported };
}
