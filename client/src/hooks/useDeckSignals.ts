import { useEffect, useRef, useState } from "react";

import type { GameFormat } from "../adapter/types";
import { expandParsedDeck, type ParsedDeck } from "../services/deckParser";
import { getDeckSignals, type DeckSignalsOutcome } from "../services/deckSignals";
import { isCommanderFamilyFormat, UNDECLARED_COMBO } from "../types/bracket";
import type { DeckSignals } from "../types/deckSignals";
import { buildBracketDeckKey } from "./bracketDeckKey";

const DEBOUNCE_MS = 200;
const CACHE_MAX_ENTRIES = 256;
const cache = new Map<string, Promise<DeckSignalsOutcome>>();

function readCacheOrFetch(
  deckKey: string,
  fetcher: () => Promise<DeckSignalsOutcome>,
): Promise<DeckSignalsOutcome> {
  const cached = cache.get(deckKey);
  if (cached) return cached;
  const promise = fetcher();
  cache.set(deckKey, promise);
  if (cache.size > CACHE_MAX_ENTRIES) {
    const firstKey = cache.keys().next().value;
    if (firstKey !== undefined) cache.delete(firstKey);
  }
  return promise;
}

export function clearDeckSignalsCache(): void {
  cache.clear();
}

interface Options {
  deck: ParsedDeck;
  commanders: string[];
  format: GameFormat | undefined;
}

interface Result {
  signals: DeckSignals | null;
  loading: boolean;
  outcome: DeckSignalsOutcome | null;
}

/** Live, debounced engine readings for the current Commander-family deck. */
export function useDeckSignals({ deck, commanders, format }: Options): Result {
  const [signals, setSignals] = useState<DeckSignals | null>(null);
  const [loading, setLoading] = useState(false);
  const [outcome, setOutcome] = useState<DeckSignalsOutcome | null>(null);
  const storedKeyRef = useRef<string | null>(null);
  const pendingKeyRef = useRef<string | null>(null);

  const eligible = isCommanderFamilyFormat(format) && commanders.length > 0;
  const deckKey = eligible
    // Signals do not depend on a declared tier or combo declaration; fixed neutral
    // values let this reuse the bracket deck-content key without under-keying.
    ? `signals:${buildBracketDeckKey(commanders, deck, null, UNDECLARED_COMBO)}`
    : null;

  useEffect(() => {
    if (!eligible || !deckKey) {
      setSignals(null);
      setLoading(false);
      setOutcome(null);
      storedKeyRef.current = null;
      pendingKeyRef.current = null;
      return;
    }
    if (deckKey === storedKeyRef.current) return;

    pendingKeyRef.current = deckKey;
    setLoading(true);
    setOutcome(null);
    const scheduledKey = deckKey;
    const timer = setTimeout(async () => {
      try {
        const expanded = expandParsedDeck(deck);
        const result = await readCacheOrFetch(scheduledKey, () =>
          getDeckSignals({
            commander: commanders,
            main_deck: expanded.main_deck,
            sideboard: expanded.sideboard,
            companion: expanded.companion,
            signature_spell: expanded.signature_spell,
            combo_declaration: UNDECLARED_COMBO,
          }),
        );
        if (pendingKeyRef.current !== scheduledKey) return;
        storedKeyRef.current = scheduledKey;
        setOutcome(result);
        setSignals(result.kind === "signals" ? result.signals : null);
      } catch {
        if (pendingKeyRef.current !== scheduledKey) return;
        setSignals(null);
        setOutcome(null);
      } finally {
        if (pendingKeyRef.current === scheduledKey) setLoading(false);
      }
    }, DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [eligible, deckKey]); // eslint-disable-line react-hooks/exhaustive-deps

  return { signals, loading, outcome };
}
