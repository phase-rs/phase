import { useEffect, useState } from "react";
import { useNavigate } from "react-router";

import { CardCoverageDashboard } from "../components/controls/CardCoverageDashboard";
import { ACTIVE_DECK_KEY, STORAGE_KEY_PREFIX } from "../constants/storage";
import { STARTER_DECKS } from "../data/starterDecks";
import type { ParsedDeck, DeckEntry } from "../services/deckParser";
import {
  loadActiveGame,
  clearActiveGame,
  clearGame,
  saveActiveGame,
  useGameStore,
} from "../stores/gameStore";
import type { ActiveGameMeta } from "../stores/gameStore";

const DIFFICULTIES = [
  { id: "VeryEasy", label: "Very Easy" },
  { id: "Easy", label: "Easy" },
  { id: "Medium", label: "Medium" },
  { id: "Hard", label: "Hard" },
  { id: "VeryHard", label: "Very Hard" },
] as const;

/** Map WUBRG color codes to Tailwind color classes. */
const COLOR_DOT_CLASS: Record<string, string> = {
  W: "bg-amber-200",
  U: "bg-blue-400",
  B: "bg-gray-600",
  R: "bg-red-500",
  G: "bg-green-500",
};

function totalCards(entries: DeckEntry[]): number {
  return entries.reduce((sum, e) => sum + e.count, 0);
}

/** Scan localStorage for saved deck names. */
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

/** Seed starter decks into localStorage if no decks exist. */
function seedStarterDecks(): void {
  for (const starter of STARTER_DECKS) {
    const deck: ParsedDeck = { main: starter.cards, sideboard: [] };
    localStorage.setItem(STORAGE_KEY_PREFIX + starter.name, JSON.stringify(deck));
  }
}

/** Get color identity for a saved deck by checking starter deck data or returning empty. */
function getDeckColorIdentity(deckName: string): string[] {
  const starter = STARTER_DECKS.find((s) => s.name === deckName);
  return starter?.colorIdentity ?? [];
}

/** Get card count for a saved deck. */
function getDeckCardCount(deckName: string): number {
  const raw = localStorage.getItem(STORAGE_KEY_PREFIX + deckName);
  if (!raw) return 0;
  const deck = JSON.parse(raw) as ParsedDeck;
  return totalCards(deck.main);
}

function startNewGame(
  navigate: ReturnType<typeof useNavigate>,
  difficulty: string,
): void {
  const gameId = crypto.randomUUID();
  const meta: ActiveGameMeta = { id: gameId, mode: "ai", difficulty };
  saveActiveGame(meta);

  // Set gameId in store before navigating so GamePage sees a match
  // and doesn't redirect back. GameProvider handles actual initialization.
  useGameStore.setState({ gameId });

  navigate(`/game/${gameId}?mode=ai&difficulty=${difficulty}`);
}

function resumeGame(
  navigate: ReturnType<typeof useNavigate>,
  meta: ActiveGameMeta,
): void {
  // Set gameId in store so GamePage sees a match and doesn't redirect
  useGameStore.setState({ gameId: meta.id });
  navigate(`/game/${meta.id}?mode=${meta.mode}&difficulty=${meta.difficulty}`);
}

type MenuView = "main" | "difficulty" | "online" | "join";

export function MenuPage() {
  const navigate = useNavigate();
  const [showCoverage, setShowCoverage] = useState(false);
  const [menuView, setMenuView] = useState<MenuView>("main");
  const [joinCode, setJoinCode] = useState("");
  const [activeDeckName, setActiveDeckName] = useState<string | null>(null);
  const [savedDeckNames, setSavedDeckNames] = useState<string[]>([]);
  const [activeGame, setActiveGame] = useState<ActiveGameMeta | null>(null);

  // On mount: seed starter decks if needed, scan for saved decks, read active deck
  useEffect(() => {
    let names = listSavedDeckNames();
    if (names.length === 0) {
      seedStarterDecks();
      names = listSavedDeckNames();
    }
    setSavedDeckNames(names);
    setActiveDeckName(localStorage.getItem(ACTIVE_DECK_KEY));
    setActiveGame(loadActiveGame());
  }, []);

  const handleSelectDeck = (name: string) => {
    setActiveDeckName(name);
    localStorage.setItem(ACTIVE_DECK_KEY, name);
  };

  const handleJoinSubmit = () => {
    const code = joinCode.trim().toUpperCase();
    if (code) {
      const gameId = crypto.randomUUID();
      useGameStore.setState({ gameId });
      navigate(`/game/${gameId}?mode=join&code=${code}`);
    }
  };

  const noDeckSelected = activeDeckName == null;
  const hasSavedGame = activeGame !== null;

  return (
    <div className="flex min-h-screen flex-col items-center justify-center gap-8">
      <h1 className="text-5xl font-bold tracking-tight">Forge.rs</h1>
      <p className="text-gray-400">Magic: The Gathering Engine</p>

      {/* Deck tile selector */}
      {savedDeckNames.length > 0 ? (
        <div className="w-full max-w-2xl px-4">
          <p className="mb-2 text-center text-xs font-medium uppercase tracking-wider text-gray-500">
            Select Deck
          </p>
          <div className="flex gap-3 overflow-x-auto pb-2">
            {savedDeckNames.map((name) => {
              const isActive = name === activeDeckName;
              const colors = getDeckColorIdentity(name);
              const count = getDeckCardCount(name);
              return (
                <button
                  key={name}
                  onClick={() => handleSelectDeck(name)}
                  className={`flex shrink-0 flex-col items-start gap-1 rounded-lg px-4 py-3 text-left transition ${
                    isActive
                      ? "bg-gray-800 ring-2 ring-indigo-500"
                      : "bg-gray-800/50 hover:bg-gray-800"
                  }`}
                >
                  <span className="text-sm font-semibold text-white">{name}</span>
                  <div className="flex items-center gap-2">
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
                    <span className="text-xs text-gray-400">{count} cards</span>
                  </div>
                </button>
              );
            })}
          </div>
        </div>
      ) : (
        <div className="text-center">
          <p className="mb-2 text-sm text-gray-400">No decks yet</p>
          <button
            onClick={() => navigate("/deck-builder")}
            className="rounded-lg bg-indigo-600 px-6 py-2 text-sm font-semibold transition-colors hover:bg-indigo-500"
          >
            Build a Deck
          </button>
        </div>
      )}

      <div className="flex flex-col gap-4">
        {menuView === "main" && (
          <>
            {hasSavedGame && (
              <button
                onClick={() => resumeGame(navigate, activeGame!)}
                className="rounded-lg bg-amber-600 px-8 py-3 text-lg font-semibold transition-colors hover:bg-amber-500"
              >
                Resume Game
              </button>
            )}
            <button
              onClick={() => {
                if (activeGame) {
                  clearGame(activeGame.id);
                  clearActiveGame();
                  setActiveGame(null);
                }
                setMenuView("difficulty");
              }}
              disabled={noDeckSelected}
              className={`rounded-lg px-8 py-3 text-lg font-semibold transition-colors ${
                noDeckSelected
                  ? "cursor-not-allowed bg-indigo-600 opacity-50"
                  : "bg-indigo-600 hover:bg-indigo-500"
              }`}
            >
              {hasSavedGame ? "New Game vs AI" : "Play vs AI"}
            </button>
            <button
              onClick={() => setMenuView("online")}
              disabled={noDeckSelected}
              className={`rounded-lg px-8 py-3 text-lg font-semibold transition-colors ${
                noDeckSelected
                  ? "cursor-not-allowed bg-emerald-600 opacity-50"
                  : "bg-emerald-600 hover:bg-emerald-500"
              }`}
            >
              Play Online
            </button>
            <button
              onClick={() => navigate("/deck-builder")}
              className="rounded-lg border border-gray-600 px-8 py-3 text-lg font-semibold transition-colors hover:border-gray-400"
            >
              Deck Builder
            </button>
            <button
              onClick={() => setShowCoverage(true)}
              className="rounded-lg border border-gray-600 px-8 py-3 text-lg font-semibold transition-colors hover:border-gray-400"
            >
              Card Coverage
            </button>
          </>
        )}

        {menuView === "difficulty" && (
          <div className="flex flex-col items-center gap-3">
            <p className="text-sm font-medium text-gray-300">
              Select Difficulty
            </p>
            <div className="flex flex-col gap-2">
              {DIFFICULTIES.map((d) => (
                <button
                  key={d.id}
                  onClick={() => startNewGame(navigate, d.id)}
                  className="rounded-lg bg-indigo-600 px-8 py-2 text-base font-semibold transition-colors hover:bg-indigo-500"
                >
                  {d.label}
                </button>
              ))}
            </div>
            <button
              onClick={() => setMenuView("main")}
              className="mt-1 text-sm text-gray-400 hover:text-gray-200"
            >
              Back
            </button>
          </div>
        )}

        {menuView === "online" && (
          <div className="flex flex-col items-center gap-3">
            <p className="text-sm font-medium text-gray-300">
              Multiplayer
            </p>
            <button
              onClick={() => {
                const gameId = crypto.randomUUID();
                useGameStore.setState({ gameId });
                navigate(`/game/${gameId}?mode=host`);
              }}
              className="rounded-lg bg-emerald-600 px-8 py-3 text-base font-semibold transition-colors hover:bg-emerald-500"
            >
              Host Game
            </button>
            <button
              onClick={() => setMenuView("join")}
              className="rounded-lg bg-cyan-600 px-8 py-3 text-base font-semibold transition-colors hover:bg-cyan-500"
            >
              Join Game
            </button>
            <button
              onClick={() => setMenuView("main")}
              className="mt-1 text-sm text-gray-400 hover:text-gray-200"
            >
              Back
            </button>
          </div>
        )}

        {menuView === "join" && (
          <div className="flex flex-col items-center gap-3">
            <p className="text-sm font-medium text-gray-300">
              Enter Game Code
            </p>
            <input
              type="text"
              value={joinCode}
              onChange={(e) => setJoinCode(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && handleJoinSubmit()}
              placeholder="e.g. ABC123"
              maxLength={10}
              className="w-48 rounded-lg bg-gray-800 px-4 py-2 text-center text-lg font-mono tracking-widest text-white placeholder-gray-500 outline-none ring-1 ring-gray-700 focus:ring-cyan-500"
              autoFocus
            />
            <button
              onClick={handleJoinSubmit}
              disabled={!joinCode.trim()}
              className={`rounded-lg px-8 py-2 text-base font-semibold transition-colors ${
                joinCode.trim()
                  ? "bg-cyan-600 text-white hover:bg-cyan-500"
                  : "cursor-not-allowed bg-gray-700 text-gray-500"
              }`}
            >
              Join
            </button>
            <button
              onClick={() => setMenuView("online")}
              className="mt-1 text-sm text-gray-400 hover:text-gray-200"
            >
              Back
            </button>
          </div>
        )}
      </div>

      {showCoverage && (
        <CardCoverageDashboard onClose={() => setShowCoverage(false)} />
      )}
    </div>
  );
}
