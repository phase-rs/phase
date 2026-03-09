import { useState, useCallback } from "react";
import { useNavigate } from "react-router";
import type { ScryfallCard } from "../../services/scryfall";
import type { ParsedDeck } from "../../services/deckParser";
import { STORAGE_KEY_PREFIX } from "../../constants/storage";
import { CardSearch } from "./CardSearch";
import { CardGrid } from "./CardGrid";
import { DeckList } from "./DeckList";
import { ManaCurve } from "./ManaCurve";

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
}

export function DeckBuilder({ onCardHover }: DeckBuilderProps) {
  const navigate = useNavigate();
  const [deck, setDeck] = useState<ParsedDeck>({ main: [], sideboard: [] });
  const [searchResults, setSearchResults] = useState<ScryfallCard[]>([]);
  const [deckName, setDeckName] = useState("");
  const [savedDecks, setSavedDecks] = useState(listSavedDecks);

  // Track Scryfall card data for CMC/color stats
  const [cardDataCache, setCardDataCache] = useState<Map<string, ScryfallCard>>(
    new Map(),
  );

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
      // Enforce 4-copy limit for non-basic-lands
      const basicLands = new Set([
        "Plains",
        "Island",
        "Swamp",
        "Mountain",
        "Forest",
      ]);
      if (existing && existing.count >= 4 && !basicLands.has(card.name)) {
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
  }, []);

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
  }, []);

  const handleExport = useCallback(() => {
    // Export handled by DeckList component directly
  }, []);

  const handleSave = () => {
    if (!deckName.trim()) return;
    const data = JSON.stringify(deck);
    localStorage.setItem(STORAGE_KEY_PREFIX + deckName.trim(), data);
    setSavedDecks(listSavedDecks());
  };

  const handleLoad = (name: string) => {
    const data = localStorage.getItem(STORAGE_KEY_PREFIX + name);
    if (data) {
      setDeck(JSON.parse(data) as ParsedDeck);
      setDeckName(name);
    }
  };

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
          <CardSearch onResults={handleSearchResults} />
        </div>

        {/* Center: Card Grid */}
        <div className="min-w-0 flex-1 overflow-y-auto">
          <CardGrid
            cards={searchResults}
            onAddCard={handleAddCard}
            onCardHover={onCardHover}
          />
        </div>

        {/* Right: Deck List + Stats */}
        <div className="w-64 shrink-0 overflow-y-auto border-l border-gray-800 p-3">
          <DeckList
            deck={deck}
            onRemoveCard={handleRemoveCard}
            onImport={handleImport}
            onExport={handleExport}
            onCardHover={onCardHover}
          />
          <div className="mt-3 border-t border-gray-700 pt-3">
            <ManaCurve cmcValues={cmcValues} colorValues={colorValues} />
          </div>
        </div>
      </div>
    </div>
  );
}
