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
 * Debounced 200ms to coalesce rapid edits. Memoized by a normalized
 * deck key so re-renders with identical inputs don't fire a new call.
 *
 * The deps array intentionally omits `commanders`, `deck.main`, and
 * `deck.sideboard` directly — their contents are captured via the
 * stable `deckKey` string, which changes only when contents actually
 * differ. This keeps the effect stable across object-identity churn
 * while still reacting to real mutations.
 */
export function useBracketEstimate({
  deck,
  commanders,
  format,
  adapter,
}: Options): Result {
  const [estimate, setEstimate] = useState<BracketEstimate | null>(null);
  const [loading, setLoading] = useState(false);
  const lastKeyRef = useRef<string | null>(null);

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
      lastKeyRef.current = null;
      return;
    }
    if (deckKey === lastKeyRef.current) return;
    setLoading(true);
    // Capture the key at effect-scheduling time so we can detect staleness
    // after the async gap.
    const scheduledKey = deckKey;
    const timer = setTimeout(async () => {
      try {
        const result = await adapter.estimateBracket({
          commander: commanders,
          main_deck: deck.main.flatMap((e) => Array(e.count).fill(e.name)),
          sideboard: deck.sideboard.flatMap((e) => Array(e.count).fill(e.name)),
        });
        // Stale-result guard: if deckKey changed while we were awaiting,
        // the new effect invocation owns the state update — discard this one.
        if (lastKeyRef.current !== null && lastKeyRef.current !== scheduledKey) {
          return;
        }
        lastKeyRef.current = scheduledKey;
        setEstimate(result);
      } catch {
        setEstimate(null);
      } finally {
        setLoading(false);
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
