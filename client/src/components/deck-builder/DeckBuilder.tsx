import { useState, useCallback, useEffect } from "react";
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
}

const BASIC_LANDS = new Set([
  "Plains",
  "Island",
  "Swamp",
  "Mountain",
  "Forest",
]);

export function DeckBuilder({
  onCardHover,
  format,
  onFormatChange,
  initialDeckName = null,
  backPath = "/",
}: DeckBuilderProps) {
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

  const handleSave = () => {
    if (!deckName.trim()) return;
    const data = JSON.stringify({ ...deck, commander: commanders, format });
    localStorage.setItem(STORAGE_KEY_PREFIX + deckName.trim(), data);
    setSavedDecks(listSavedDecks());
  };

  const handleLoad = useCallback((name: string) => {
    const data = localStorage.getItem(STORAGE_KEY_PREFIX + name);
    if (data) {
      const parsed = JSON.parse(data);
      setDeck({ main: deduplicateEntries(parsed.main ?? []), sideboard: deduplicateEntries(parsed.sideboard ?? []) });
      setCommanders(parsed.commander ?? []);
      if (parsed.format) onFormatChange(parsed.format);
      setDeckName(name);
    }
  }, [onFormatChange]);

  useEffect(() => {
    if (!initialDeckName) return;
    handleLoad(initialDeckName);
  }, [initialDeckName, handleLoad]);

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
          <CardSearch onResults={handleSearchResults} format={format} />
        </div>

        <div className="min-w-0 flex-1 overflow-y-auto">
          <CardGrid
            cards={searchResults}
            onAddCard={handleAddCard}
            onCardHover={onCardHover}
            format={format}
          />
        </div>

        <div className="w-64 shrink-0 overflow-y-auto border-l border-white/8 bg-black/12 p-3 backdrop-blur-sm">
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
            deck={deck}
            onRemoveCard={handleRemoveCard}
            onImport={handleImport}
            onCardHover={onCardHover}
            warnings={warnings}
            format={format}
          />
          <div className="mt-3 border-t border-white/8 pt-3">
            <ManaCurve cmcValues={cmcValues} colorValues={colorValues} />
          </div>
        </div>
      </div>
    </div>
  );
}
