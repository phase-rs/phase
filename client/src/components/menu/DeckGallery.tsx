import { useEffect, useState } from "react";

import { ACTIVE_DECK_KEY, STORAGE_KEY_PREFIX } from "../../constants/storage";
import { STARTER_DECKS } from "../../data/starterDecks";
import { useCardImage } from "../../hooks/useCardImage";
import type { ParsedDeck } from "../../services/deckParser";

const DIFFICULTIES = [
  { id: "VeryEasy", label: "Very Easy" },
  { id: "Easy", label: "Easy" },
  { id: "Medium", label: "Medium" },
  { id: "Hard", label: "Hard" },
  { id: "VeryHard", label: "Very Hard" },
] as const;

const BASIC_LANDS = new Set(["Plains", "Island", "Swamp", "Mountain", "Forest"]);

const COLOR_DOT_CLASS: Record<string, string> = {
  W: "bg-amber-200",
  U: "bg-blue-400",
  B: "bg-gray-600",
  R: "bg-red-500",
  G: "bg-green-500",
};

interface DeckGalleryProps {
  onSelectDeck: (deckName: string) => void;
  activeDeckName: string | null;
  mode: "ai" | "online";
  difficulty: string;
  onDifficultyChange: (d: string) => void;
  onStartGame: () => void;
  onBack: () => void;
}

function listSavedDeckNames(): string[] {
  const names: string[] = [];
  for (let i = 0; i < localStorage.length; i++) {
    const key = localStorage.key(i);
    if (key?.startsWith(STORAGE_KEY_PREFIX)) {
      names.push(key.slice(STORAGE_KEY_PREFIX.length));
    }
  }
  return names.sort();
}

function getDeckColorIdentity(deckName: string): string[] {
  const starter = STARTER_DECKS.find((s) => s.name === deckName);
  return starter?.colorIdentity ?? [];
}

function getDeckCardCount(deckName: string): number {
  const raw = localStorage.getItem(STORAGE_KEY_PREFIX + deckName);
  if (!raw) return 0;
  const deck = JSON.parse(raw) as ParsedDeck;
  return deck.main.reduce((sum, e) => sum + e.count, 0);
}

/** Find the first non-basic-land card name in a deck for art display. */
function getRepresentativeCard(deckName: string): string | null {
  const raw = localStorage.getItem(STORAGE_KEY_PREFIX + deckName);
  if (!raw) return null;
  const deck = JSON.parse(raw) as ParsedDeck;
  const entry = deck.main.find((e) => !BASIC_LANDS.has(e.name));
  return entry?.name ?? null;
}

function DeckArtTile({ cardName }: { cardName: string | null }) {
  const { src, isLoading } = useCardImage(cardName ?? "", { size: "art_crop" });

  if (!cardName || isLoading || !src) {
    return (
      <div className="absolute inset-0 animate-pulse bg-gray-800" />
    );
  }

  return (
    <img
      src={src}
      alt=""
      className="absolute inset-0 h-full w-full object-cover"
    />
  );
}

export function DeckGallery({
  onSelectDeck,
  activeDeckName,
  mode,
  difficulty,
  onDifficultyChange,
  onStartGame,
  onBack,
}: DeckGalleryProps) {
  const [deckNames, setDeckNames] = useState<string[]>([]);

  useEffect(() => {
    setDeckNames(listSavedDeckNames());
  }, []);

  // Restore last-used deck on mount
  useEffect(() => {
    if (activeDeckName == null) {
      const stored = localStorage.getItem(ACTIVE_DECK_KEY);
      if (stored && deckNames.includes(stored)) {
        onSelectDeck(stored);
      }
    }
  }, [activeDeckName, deckNames, onSelectDeck]);

  const noDeckSelected = activeDeckName == null;

  return (
    <div className="flex w-full max-w-3xl flex-col items-center gap-6 px-4">
      <h2 className="text-2xl font-bold tracking-tight">Select Deck</h2>

      {mode === "ai" && (
        <div className="flex overflow-hidden rounded-lg border border-gray-700">
          {DIFFICULTIES.map((d) => (
            <button
              key={d.id}
              onClick={() => onDifficultyChange(d.id)}
              className={`px-3 py-1.5 text-xs font-medium transition-colors ${
                difficulty === d.id
                  ? "bg-indigo-600 text-white"
                  : "bg-gray-800 text-gray-400 hover:bg-gray-700 hover:text-gray-200"
              }`}
            >
              {d.label}
            </button>
          ))}
        </div>
      )}

      <div className="grid w-full grid-cols-2 gap-4 sm:grid-cols-3 lg:grid-cols-4">
        {deckNames.map((name) => {
          const isActive = name === activeDeckName;
          const colors = getDeckColorIdentity(name);
          const count = getDeckCardCount(name);
          const representativeCard = getRepresentativeCard(name);

          return (
            <button
              key={name}
              onClick={() => onSelectDeck(name)}
              className={`group relative flex aspect-[4/3] flex-col justify-end overflow-hidden rounded-xl transition ${
                isActive
                  ? "ring-2 ring-indigo-500 ring-offset-2 ring-offset-gray-950"
                  : "ring-1 ring-gray-700 hover:ring-gray-500"
              }`}
            >
              <DeckArtTile cardName={representativeCard} />

              <div className="relative z-10 bg-gradient-to-t from-black/90 via-black/60 to-transparent px-3 pb-3 pt-8">
                <p className="text-sm font-semibold text-white">{name}</p>
                <div className="mt-1 flex items-center gap-2">
                  <div className="flex gap-1">
                    {colors.map((c) => (
                      <span
                        key={c}
                        className={`inline-block h-2.5 w-2.5 rounded-full ${COLOR_DOT_CLASS[c] ?? "bg-gray-400"}`}
                      />
                    ))}
                    {colors.length === 0 && (
                      <span className="inline-block h-2.5 w-2.5 rounded-full bg-gray-500" />
                    )}
                  </div>
                  <span className="text-xs text-gray-300">{count} cards</span>
                </div>
              </div>
            </button>
          );
        })}
      </div>

      <div className="flex items-center gap-4">
        <button
          onClick={onBack}
          className="rounded-lg border border-gray-600 px-6 py-2.5 text-sm font-semibold transition-colors hover:border-gray-400"
        >
          Back
        </button>

        <button
          onClick={onStartGame}
          disabled={noDeckSelected}
          className={`rounded-lg px-8 py-2.5 text-sm font-semibold transition-colors ${
            noDeckSelected
              ? "cursor-not-allowed bg-indigo-600 opacity-50"
              : "bg-indigo-600 hover:bg-indigo-500"
          }`}
        >
          {mode === "ai" ? "Start Game" : "Host Game"}
        </button>
      </div>
    </div>
  );
}
