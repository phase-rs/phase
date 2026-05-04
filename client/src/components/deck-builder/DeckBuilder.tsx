import { useState, useCallback, useEffect, useMemo } from "react";
import { useNavigate } from "react-router";
import type { ScryfallCard } from "../../services/scryfall";
import type { ParsedDeck } from "../../services/deckParser";
import { deduplicateEntries, resolveCommander } from "../../services/deckParser";
import { evaluateDeckCompatibility, type DeckCompatibilityResult } from "../../services/deckCompatibility";
import { STORAGE_KEY_PREFIX, loadSavedDeck, stampDeckMeta } from "../../constants/storage";
import { BASIC_LAND_NAMES } from "../../constants/game";
import { useDeckCardData } from "../../hooks/useDeckCardData";
import { CardSearch } from "./CardSearch";
import type { CardSearchFilters } from "./CardSearch";
import { CardGrid } from "./CardGrid";
import { DeckStack } from "./DeckStack";
import { DeckList } from "./DeckList";
import { ManaCurve } from "./ManaCurve";
import { FormatFilter, type DeckFormat } from "./FormatFilter";
import { CommanderPanel } from "./CommanderPanel";
import {
  getColorIdentityViolations,
  getSingletonViolations,
  canBeCommander,
  canAddPartner,
} from "./commanderUtils";

function listSavedDecks(): string[] {
  const keys: string[] = [];
  for (let i = 0; i < localStorage.length; i++) {
    const key = localStorage.key(i);
    if (key?.startsWith(STORAGE_KEY_PREFIX)) {
      keys.push(key.slice(STORAGE_KEY_PREFIX.length));
    }
  }
  return keys.sort();
}

interface DeckBuilderProps {
  onCardHover?: (cardName: string | null) => void;
  format: DeckFormat;
  onFormatChange: (format: DeckFormat) => void;
  initialDeckName?: string | null;
  backPath?: string;
  searchFilters: CardSearchFilters;
  onSearchFiltersChange: (filters: CardSearchFilters) => void;
  onResetSearch: () => void;
}


export function DeckBuilder({
  onCardHover,
  format,
  onFormatChange,
  initialDeckName = null,
  backPath = "/",
  searchFilters,
  onSearchFiltersChange,
  onResetSearch,
}: DeckBuilderProps) {
  const navigate = useNavigate();
  const [deck, setDeck] = useState<ParsedDeck>({ main: [], sideboard: [] });
  const [searchResults, setSearchResults] = useState<ScryfallCard[]>([]);
  const [deckName, setDeckName] = useState("");
  const [savedDecks, setSavedDecks] = useState(listSavedDecks);
  const [commanders, setCommanders] = useState<string[]>([]);
  const [isDeckViewExpanded, setIsDeckViewExpanded] = useState(true);
  const { cardDataCache, cacheCards } = useDeckCardData([
    ...deck.main.map((entry) => entry.name),
    ...deck.sideboard.map((entry) => entry.name),
    ...commanders,
  ]);

  const [compatibility, setCompatibility] = useState<DeckCompatibilityResult | null>(null);
  const currentDeck = useMemo<ParsedDeck>(() => ({
    ...deck,
    commander: commanders.length > 0 ? commanders : undefined,
  }), [deck, commanders]);

  // Stable key for deck contents to debounce compatibility evaluation
  const deckKey = useMemo(
    () => [
      ...deck.main.map((e) => `${e.count}x${e.name}`),
      "//",
      ...deck.sideboard.map((e) => `${e.count}x${e.name}`),
      "//",
      ...commanders,
    ].join("|"),
    [deck, commanders],
  );

  useEffect(() => {
    if (currentDeck.main.length === 0 && currentDeck.sideboard.length === 0) {
      setCompatibility(null);
      return;
    }
    let cancelled = false;
    const timer = setTimeout(() => {
      evaluateDeckCompatibility(currentDeck).then((result) => {
        if (!cancelled) setCompatibility(result);
      }).catch(() => {
        // WASM may not be loaded yet; silently ignore
      });
    }, 300);
    return () => { cancelled = true; clearTimeout(timer); };
  }, [currentDeck, deckKey]);

  const isCommander = format === "commander";
  const maxCopies = isCommander ? 1 : 4;

  const handleSearchResults = useCallback(
    (cards: ScryfallCard[], _total: number) => {
      setIsDeckViewExpanded(false);
      setSearchResults(cards);
      cacheCards(cards);
    },
    [cacheCards],
  );

  const handleSearchTrigger = useCallback(() => {
    setIsDeckViewExpanded(false);
  }, []);

  const handleAddCard = useCallback((card: ScryfallCard) => {
    cacheCards([card]);

    setDeck((prev) => {
      const existing = prev.main.find((e) => e.name === card.name);
      if (existing && existing.count >= maxCopies && !BASIC_LAND_NAMES.has(card.name)) {
        return prev;
      }

      if (existing) {
        return {
          ...prev,
          main: prev.main.map((e) =>
            e.name === card.name ? { ...e, count: e.count + 1 } : e,
          ),
        };
      }
      return {
        ...prev,
        main: [...prev.main, { count: 1, name: card.name }],
      };
    });
  }, [cacheCards, maxCopies]);

  const handleAddCardByName = useCallback((name: string) => {
    const card = cardDataCache.get(name);
    if (!card) return;
    handleAddCard(card);
  }, [cardDataCache, handleAddCard]);

  const handleRemoveCard = useCallback(
    (name: string, section: "main" | "sideboard") => {
      setDeck((prev) => {
        const entries = prev[section];
        const existing = entries.find((e) => e.name === name);
        if (!existing) return prev;

        if (existing.count <= 1) {
          return {
            ...prev,
            [section]: entries.filter((e) => e.name !== name),
          };
        }
        return {
          ...prev,
          [section]: entries.map((e) =>
            e.name === name ? { ...e, count: e.count - 1 } : e,
          ),
        };
      });
    },
    [],
  );

  const handleMoveCard = useCallback(
    (name: string, from: "main" | "sideboard") => {
      const to: "main" | "sideboard" = from === "main" ? "sideboard" : "main";
      setDeck((prev) => {
        const source = prev[from];
        const target = prev[to];
        const sourceEntry = source.find((e) => e.name === name);
        if (!sourceEntry) return prev;

        const targetEntry = target.find((e) => e.name === name);
        if (
          to === "main" &&
          targetEntry &&
          targetEntry.count >= maxCopies &&
          !BASIC_LAND_NAMES.has(name)
        ) {
          return prev;
        }

        const nextSource =
          sourceEntry.count <= 1
            ? source.filter((e) => e.name !== name)
            : source.map((e) =>
                e.name === name ? { ...e, count: e.count - 1 } : e,
              );

        const nextTarget = targetEntry
          ? target.map((e) =>
              e.name === name ? { ...e, count: e.count + 1 } : e,
            )
          : [...target, { count: 1, name }];

        return {
          ...prev,
          [from]: nextSource,
          [to]: nextTarget,
        };
      });
    },
    [maxCopies],
  );

  const applyDeckToEditor = useCallback((next: ParsedDeck) => {
    setDeck({
      main: deduplicateEntries(next.main ?? []),
      sideboard: deduplicateEntries(next.sideboard ?? []),
      companion: next.companion,
    });
    setCommanders(next.commander ?? []);
    if (next.commander?.length) onFormatChange("commander");
  }, [onFormatChange]);

  const handleImport = useCallback((imported: ParsedDeck) => {
    applyDeckToEditor(imported);
  }, [applyDeckToEditor]);

  const handleSave = () => {
    if (!deckName.trim()) return;
    const data = JSON.stringify({ ...currentDeck, format });
    localStorage.setItem(STORAGE_KEY_PREFIX + deckName.trim(), data);
    stampDeckMeta(deckName.trim());
    setSavedDecks(listSavedDecks());
  };

  const handleLoad = useCallback(async (name: string) => {
    const parsed = loadSavedDeck(name);
    const data = localStorage.getItem(STORAGE_KEY_PREFIX + name);
    if (!parsed || !data) return;
    const persisted = JSON.parse(data) as ParsedDeck & { format?: DeckFormat };
    const resolved = await resolveCommander(parsed);
    applyDeckToEditor(resolved);
    if (persisted.format) {
      onFormatChange(persisted.format);
    } else if (resolved.commander?.length) {
      onFormatChange("commander");
    }
    setDeckName(name);
  }, [applyDeckToEditor, onFormatChange]);

  useEffect(() => {
    if (!initialDeckName) return;
    void handleLoad(initialDeckName);
  }, [initialDeckName, handleLoad]);

  useEffect(() => {
    if (!isCommander || commanders.length > 0) return;
    let cancelled = false;
    resolveCommander(deck)
      .then((resolved) => {
        if (cancelled || !resolved.commander?.length) return;
        applyDeckToEditor(resolved);
      })
      .catch(() => {
        // WASM may not be loaded yet; leave the deck unchanged.
      });
    return () => {
      cancelled = true;
    };
  }, [applyDeckToEditor, commanders.length, deck, isCommander]);

  const handleSetCommander = useCallback(
    (cardName: string) => {
      setCommanders((prev) => {
        if (prev.includes(cardName)) return prev;
        const card = cardDataCache.get(cardName);
        if (!card || !canBeCommander(card)) return prev;
        if (!canAddPartner(prev, card, cardDataCache)) return prev;
        return [...prev, cardName];
      });
      // Remove from main deck
      setDeck((prev) => ({
        ...prev,
        main: prev.main.filter((e) => e.name !== cardName),
      }));
    },
    [cardDataCache],
  );

  const handleRemoveCommander = useCallback((cardName: string) => {
    setCommanders((prev) => prev.filter((n) => n !== cardName));
    // Add back to main deck
    setDeck((prev) => ({
      ...prev,
      main: [...prev.main, { count: 1, name: cardName }],
    }));
  }, []);

  // Compute CMC and color arrays for ManaCurve
  const cmcValues: number[] = [];
  const colorValues: string[] = [];
  for (const entry of deck.main) {
    const card = cardDataCache.get(entry.name);
    if (card) {
      for (let i = 0; i < entry.count; i++) {
        cmcValues.push(card.cmc);
        colorValues.push(card.color_identity?.join("") ?? "");
      }
    }
  }

  const cardCounts = new Map(deck.main.map((entry) => [entry.name, entry.count]));
  for (const commander of commanders) {
    cardCounts.set(commander, (cardCounts.get(commander) ?? 0) + 1);
  }

  // Compute validation warnings
  const warnings: string[] = [];
  if (isCommander) {
    const totalCards = deck.main.reduce((s, e) => s + e.count, 0) + commanders.length;
    if (totalCards > 0 && totalCards !== 100) {
      warnings.push(`Deck has ${totalCards} cards (need exactly 100)`);
    }
    for (const name of getSingletonViolations(deck.main)) {
      warnings.push(`${name}: multiple copies (singleton format)`);
    }
    for (const name of getColorIdentityViolations(deck.main, commanders, cardDataCache)) {
      warnings.push(`${name}: outside commander color identity`);
    }
  } else {
    const mainTotal = deck.main.reduce((s, e) => s + e.count, 0);
    if (mainTotal > 0 && mainTotal < 60) {
      warnings.push(`Deck has ${mainTotal} cards (minimum 60)`);
    }
    for (const entry of deck.main) {
      if (entry.count > 4 && !BASIC_LAND_NAMES.has(entry.name)) {
        warnings.push(`${entry.name}: ${entry.count} copies (max 4)`);
      }
    }
  }
  // CR 702.139a: Warn if a companion card is also in the main deck (likely import error)
  if (deck.companion && deck.main.some((e) => e.name === deck.companion)) {
    warnings.push(
      `${deck.companion} is your companion but is also in the main deck — this violates its deckbuilding condition. Remove it from the main deck to use it as a companion.`,
    );
  }

  return (
    <div className="flex h-screen flex-col bg-transparent">
      <div className="flex items-center justify-between border-b border-white/8 bg-black/18 px-4 py-2 backdrop-blur-md">
        <div className="flex min-w-0 items-center gap-4">
          <button
            onClick={() => navigate(backPath)}
            className="text-sm text-slate-400 hover:text-white"
          >
            &larr; Menu
          </button>
          <div className="min-w-0">
            <div className="text-[0.68rem] uppercase tracking-[0.22em] text-slate-500">Deck Builder</div>
            <div className="truncate text-sm font-medium text-white">
              {deckName.trim() || "Untitled Deck"}
            </div>
          </div>
        </div>
        <FormatFilter selected={format} onChange={onFormatChange} />
        <div className="flex items-center gap-2">
          <input
            type="text"
            value={deckName}
            onChange={(e) => setDeckName(e.target.value)}
            placeholder="Deck name..."
            className="w-40 rounded-xl border border-white/10 bg-black/18 px-3 py-1.5 text-sm text-white placeholder-gray-500 focus:border-white/20 focus:outline-none"
          />
          <button
            onClick={handleSave}
            disabled={!deckName.trim()}
            className="rounded-xl border border-white/10 bg-white/10 px-3 py-1.5 text-sm text-white hover:bg-white/14 disabled:opacity-40"
          >
            Save
          </button>
          {savedDecks.length > 0 && (
            <select
              onChange={(e) => e.target.value && handleLoad(e.target.value)}
              defaultValue=""
              className="rounded-xl border border-white/10 bg-black/18 px-3 py-1.5 text-sm text-white focus:outline-none"
            >
              <option value="">Load deck...</option>
              {savedDecks.map((name) => (
                <option key={name} value={name}>
                  {name}
                </option>
              ))}
            </select>
          )}
        </div>
      </div>

      <div className="flex min-h-0 flex-1">
        <div className="w-56 shrink-0 overflow-y-auto border-r border-white/8 bg-black/12 backdrop-blur-sm">
          <CardSearch
            onResults={handleSearchResults}
            onSearchTrigger={handleSearchTrigger}
            filters={searchFilters}
            onFiltersChange={onSearchFiltersChange}
            onReset={onResetSearch}
          />
        </div>

        <div className="flex min-w-0 flex-1 flex-col">
          {!isDeckViewExpanded && (
            <div className="min-h-0 flex-1 overflow-y-auto border-b border-white/8">
              <CardGrid
                cards={searchResults}
                onAddCard={handleAddCard}
                onCardHover={onCardHover}
                cardCounts={cardCounts}
                legalityFormat={searchFilters.browseFormat}
              />
            </div>
          )}

          <div className="flex items-center justify-end border-b border-white/8 bg-black/12 px-3 py-2">
            <button
              onClick={() => setIsDeckViewExpanded((prev) => !prev)}
              className="rounded-xl border border-white/10 bg-black/18 px-3 py-1.5 text-xs font-medium text-slate-200 hover:bg-white/6"
            >
              {isDeckViewExpanded ? "Show Browser" : "Expand Deck View"}
            </button>
          </div>

          <div className="min-h-0 flex-1 overflow-hidden bg-black/8">
            <DeckStack
              deck={deck}
              commanders={commanders}
              cardDataCache={cardDataCache}
              onAddCard={handleAddCardByName}
              onRemoveCard={handleRemoveCard}
              onRemoveCommander={handleRemoveCommander}
              onCardHover={onCardHover}
            />
          </div>
        </div>

        <div className="flex min-h-0 w-64 shrink-0 flex-col overflow-hidden border-l border-white/8 bg-black/12 p-3 backdrop-blur-sm">
          <div className="min-h-0 flex-1 overflow-y-auto pr-1">
            {isCommander && (
              <div className="mb-3 border-b border-white/8 pb-3">
                <CommanderPanel
                  commanders={commanders}
                  deck={deck.main}
                  cardDataCache={cardDataCache}
                  onSetCommander={handleSetCommander}
                  onRemoveCommander={handleRemoveCommander}
                />
              </div>
            )}
            <DeckList
              deck={currentDeck}
              onRemoveCard={handleRemoveCard}
              onMoveCard={handleMoveCard}
              onImport={handleImport}
              onCardHover={onCardHover}
              warnings={warnings}
              format={format}
              compatibility={compatibility}
            />
          </div>

          <div className="mt-3 shrink-0 rounded-[18px] border border-white/8 bg-black/18 p-3">
            <ManaCurve cmcValues={cmcValues} colorValues={colorValues} />
          </div>
        </div>
      </div>
    </div>
  );
}
