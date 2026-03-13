import { useCallback, useEffect, useMemo, useState } from "react";

import { fetchCardData, type ScryfallCard } from "../services/scryfall";

function mergeIntoCache(
  previous: Map<string, ScryfallCard>,
  cards: Iterable<ScryfallCard>,
): Map<string, ScryfallCard> {
  const next = new Map(previous);
  for (const card of cards) {
    next.set(card.name, card);
  }
  return next;
}

export function useDeckCardData(requiredCardNames: string[]) {
  const [cardDataCache, setCardDataCache] = useState<Map<string, ScryfallCard>>(
    new Map(),
  );

  const cacheCards = useCallback((cards: Iterable<ScryfallCard>) => {
    setCardDataCache((prev) => mergeIntoCache(prev, cards));
  }, []);

  const requiredNames = useMemo(
    () => [...new Set(requiredCardNames)].sort(),
    [requiredCardNames],
  );
  const requiredNamesKey = requiredNames.join("\n");

  useEffect(() => {
    const missingNames = requiredNames.filter((name) => !cardDataCache.has(name));
    if (missingNames.length === 0) return;

    let cancelled = false;

    async function hydrateMissingCards(): Promise<void> {
      const results = await Promise.allSettled(
        missingNames.map((name) => fetchCardData(name)),
      );
      if (cancelled) return;

      setCardDataCache((prev) => {
        const resolvedCards = results.flatMap((result) =>
          result.status === "fulfilled" ? [result.value] : [],
        );
        return mergeIntoCache(prev, resolvedCards);
      });
    }

    hydrateMissingCards();

    return () => {
      cancelled = true;
    };
  }, [cardDataCache, requiredNames, requiredNamesKey]);

  return { cardDataCache, cacheCards };
}
