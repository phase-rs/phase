import { useEffect, useRef, useState } from "react";

import type { BracketEstimate, EngineAdapter, GameFormat } from "../adapter/types";
import type { ParsedDeck } from "../services/deckParser";

const DEBOUNCE_MS = 200;

interface Options {
  deck: ParsedDeck;
  commanders: string[];
  format: GameFormat;
  adapter: Pick<EngineAdapter, "estimateBracket">;
}

interface Result {
  estimate: BracketEstimate | null;
  loading: boolean;
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
  /** Last successfully *stored* key — used to short-circuit identical re-renders. */
  const storedKeyRef = useRef<string | null>(null);
  /** Latest *scheduled* key — written synchronously, used as the stale-result guard. */
  const pendingKeyRef = useRef<string | null>(null);

  const eligible = format === "Commander" && commanders.length > 0;

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
      storedKeyRef.current = null;
      pendingKeyRef.current = null;
      return;
    }
    if (deckKey === storedKeyRef.current) return;

    pendingKeyRef.current = deckKey;
    setLoading(true);
    const scheduledKey = deckKey;
    const timer = setTimeout(async () => {
      try {
        const result = await adapter.estimateBracket({
          commander: commanders,
          main_deck: deck.main.flatMap((e) => Array(e.count).fill(e.name)),
          sideboard: deck.sideboard.flatMap((e) => Array(e.count).fill(e.name)),
        });
        if (pendingKeyRef.current !== scheduledKey) {
          // A newer effect superseded us; discard this stale result.
          return;
        }
        storedKeyRef.current = scheduledKey;
        setEstimate(result);
      } catch {
        if (pendingKeyRef.current === scheduledKey) {
          setEstimate(null);
        }
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

  return { estimate, loading };
}
