import { useState, useCallback } from "react";
import { useNavigate } from "react-router";
import type { ScryfallCard } from "../../services/scryfall";
import type { ParsedDeck } from "../../services/deckParser";
import { deduplicateEntries } from "../../services/deckParser";
import { STORAGE_KEY_PREFIX } from "../../constants/storage";
import { CardSearch } from "./CardSearch";
import { CardGrid } from "./CardGrid";
import { DeckList } from "./DeckList";
import { ManaCurve } from "./ManaCurve";
import { FormatFilter, type DeckFormat } from "./FormatFilter";
import {
  CommanderPanel,
  getColorIdentityViolations,
  getSingletonViolations,
  canBeCommander,
  canAddPartner,
} from "./CommanderPanel";

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
}

const BASIC_LANDS = new Set([
  "Plains",
  "Island",
  "Swamp",
  "Mountain",
  "Forest",
]);

export function DeckBuilder({ onCardHover, format, onFormatChange }: DeckBuilderProps) {
  const navigate = useNavigate();
  const [deck, setDeck] = useState<ParsedDeck>({ main: [], sideboard: [] });
  const [searchResults, setSearchResults] = useState<ScryfallCard[]>([]);
  const [deckName, setDeckName] = useState("");
  const [savedDecks, setSavedDecks] = useState(listSavedDecks);
  const [commanders, setCommanders] = useState<string[]>([]);

  // Track Scryfall card data for CMC/color stats
  const [cardDataCache, setCardDataCache] = useState<Map<string, ScryfallCard>>(
    new Map(),
  );

  const isCommander = format === "commander";
  const maxCopies = isCommander ? 1 : 4;

  const handleSearchResults = useCallback(
    (cards: ScryfallCard[], _total: number) => {
      setSearchResults(cards);
      // Cache card data for stats
      setCardDataCache((prev) => {
        const next = new Map(prev);
        for (const card of cards) {
          next.set(card.name, card);
        }
        return next;
      });
    },
    [],
  );

  const handleAddCard = useCallback((card: ScryfallCard) => {
    // Cache the card data
    setCardDataCache((prev) => new Map(prev).set(card.name, card));

    setDeck((prev) => {
      const existing = prev.main.find((e) => e.name === card.name);
      if (existing && existing.count >= maxCopies && !BASIC_LANDS.has(card.name)) {
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
  }, [maxCopies]);

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

  const handleImport = useCallback((imported: ParsedDeck) => {
    setDeck(imported);
    // Auto-detect commanders from sideboard in commander format
    if (imported.commander) {
      setCommanders(imported.commander);
    }
  }, []);

  const handleExport = useCallback(() => {
    // Export handled by DeckList component directly
  }, []);

  const handleSave = () => {
    if (!deckName.trim()) return;
    const data = JSON.stringify({ ...deck, commander: commanders, format });
    localStorage.setItem(STORAGE_KEY_PREFIX + deckName.trim(), data);
    setSavedDecks(listSavedDecks());
  };

  const handleLoad = (name: string) => {
    const data = localStorage.getItem(STORAGE_KEY_PREFIX + name);
    if (data) {
      const parsed = JSON.parse(data);
      setDeck({ main: deduplicateEntries(parsed.main ?? []), sideboard: deduplicateEntries(parsed.sideboard ?? []) });
      setCommanders(parsed.commander ?? []);
      if (parsed.format) onFormatChange(parsed.format);
      setDeckName(name);
    }
  };

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
      if (entry.count > 4 && !BASIC_LANDS.has(entry.name)) {
        warnings.push(`${entry.name}: ${entry.count} copies (max 4)`);
      }
    }
  }

  return (
    <div className="flex h-screen flex-col bg-gray-950">
      {/* Top bar */}
      <div className="flex items-center justify-between border-b border-gray-800 px-4 py-2">
        <button
          onClick={() => navigate("/")}
          className="text-sm text-gray-400 hover:text-white"
        >
          &larr; Menu
        </button>
        <FormatFilter selected={format} onChange={onFormatChange} />
        <div className="flex items-center gap-2">
          <input
            type="text"
            value={deckName}
            onChange={(e) => setDeckName(e.target.value)}
            placeholder="Deck name..."
            className="w-40 rounded border border-gray-700 bg-gray-800 px-2 py-1 text-sm text-white placeholder-gray-500 focus:border-blue-500 focus:outline-none"
          />
          <button
            onClick={handleSave}
            disabled={!deckName.trim()}
            className="rounded bg-blue-600 px-3 py-1 text-sm text-white hover:bg-blue-500 disabled:opacity-40"
          >
            Save
          </button>
          {savedDecks.length > 0 && (
            <select
              onChange={(e) => e.target.value && handleLoad(e.target.value)}
              defaultValue=""
              className="rounded border border-gray-700 bg-gray-800 px-2 py-1 text-sm text-white focus:outline-none"
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

      {/* Three-column layout */}
      <div className="flex min-h-0 flex-1">
        {/* Left: Search & Filters */}
        <div className="w-56 shrink-0 overflow-y-auto border-r border-gray-800">
          <CardSearch onResults={handleSearchResults} format={format} />
        </div>

        {/* Center: Card Grid */}
        <div className="min-w-0 flex-1 overflow-y-auto">
          <CardGrid
            cards={searchResults}
            onAddCard={handleAddCard}
            onCardHover={onCardHover}
            format={format}
          />
        </div>

        {/* Right: Deck List + Stats */}
        <div className="w-64 shrink-0 overflow-y-auto border-l border-gray-800 p-3">
          {isCommander && (
            <div className="mb-3 border-b border-gray-700 pb-3">
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
            deck={deck}
            onRemoveCard={handleRemoveCard}
            onImport={handleImport}
            onExport={handleExport}
            onCardHover={onCardHover}
            warnings={warnings}
            format={format}
          />
          <div className="mt-3 border-t border-gray-700 pt-3">
            <ManaCurve cmcValues={cmcValues} colorValues={colorValues} />
          </div>
        </div>
      </div>
    </div>
  );
}
