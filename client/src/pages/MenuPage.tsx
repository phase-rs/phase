import { useEffect, useState } from "react";
import { useNavigate } from "react-router";

import { initAudioOnInteraction } from "../audio/AudioManager";
import { CardCoverageDashboard } from "../components/controls/CardCoverageDashboard";
import { DeckGallery } from "../components/menu/DeckGallery";
import { MenuLogo } from "../components/menu/MenuLogo";
import { MenuParticles } from "../components/menu/MenuParticles";
import { ACTIVE_DECK_KEY, STORAGE_KEY_PREFIX } from "../constants/storage";
import { STARTER_DECKS } from "../data/starterDecks";
import type { ParsedDeck } from "../services/deckParser";
import {
  loadActiveGame,
  clearActiveGame,
  clearGame,
  saveActiveGame,
  useGameStore,
} from "../stores/gameStore";
import type { ActiveGameMeta } from "../stores/gameStore";

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

type MenuView =
  | "mode-select"
  | "deck-gallery-ai"
  | "deck-gallery-online"
  | "online-host-join"
  | "join-code";

export function MenuPage() {
  const navigate = useNavigate();
  const [showCoverage, setShowCoverage] = useState(false);
  const [menuView, setMenuView] = useState<MenuView>("mode-select");
  const [joinCode, setJoinCode] = useState("");
  const [activeDeckName, setActiveDeckName] = useState<string | null>(null);
  const [activeGame, setActiveGame] = useState<ActiveGameMeta | null>(null);
  const [difficulty, setDifficulty] = useState("Medium");

  // Warm up AudioContext on first user interaction
  useEffect(() => {
    initAudioOnInteraction();
  }, []);

  // On mount: seed starter decks if needed, read active deck, check active game
  useEffect(() => {
    const names = listSavedDeckNames();
    if (names.length === 0) {
      seedStarterDecks();
    }
    setActiveDeckName(localStorage.getItem(ACTIVE_DECK_KEY));

    // Auto-resume: if there's an active game, navigate straight into it
    const saved = loadActiveGame();
    if (saved) {
      useGameStore.setState({ gameId: saved.id });
      navigate(`/game/${saved.id}?mode=${saved.mode}&difficulty=${saved.difficulty}`, { replace: true });
      return;
    }
    setActiveGame(saved);
  }, [navigate]);

  const handleSelectDeck = (name: string) => {
    setActiveDeckName(name);
    localStorage.setItem(ACTIVE_DECK_KEY, name);
  };

  const handleStartAIGame = () => {
    if (activeGame) {
      clearGame(activeGame.id);
      clearActiveGame();
      setActiveGame(null);
    }
    const gameId = crypto.randomUUID();
    const meta: ActiveGameMeta = { id: gameId, mode: "ai", difficulty };
    saveActiveGame(meta);
    useGameStore.setState({ gameId });
    navigate(`/game/${gameId}?mode=ai&difficulty=${difficulty}`);
  };

  const handleHostOnlineGame = () => {
    const gameId = crypto.randomUUID();
    useGameStore.setState({ gameId });
    navigate(`/game/${gameId}?mode=host`);
  };

  const handleResumeGame = () => {
    if (!activeGame) return;
    useGameStore.setState({ gameId: activeGame.id });
    navigate(`/game/${activeGame.id}?mode=${activeGame.mode}&difficulty=${activeGame.difficulty}`);
  };

  const handleJoinSubmit = () => {
    const code = joinCode.trim().toUpperCase();
    if (code) {
      const gameId = crypto.randomUUID();
      useGameStore.setState({ gameId });
      navigate(`/game/${gameId}?mode=join&code=${code}`);
    }
  };

  const hasSavedGame = activeGame !== null;

  return (
    <div className="relative flex min-h-screen flex-col items-center justify-center">
      <MenuParticles />

      {menuView === "mode-select" && (
        <div className="relative z-10 flex flex-col items-center gap-8">
          <MenuLogo />
          <p className="text-gray-400">Magic: The Gathering Engine</p>

          <div className="flex flex-col gap-4">
            {hasSavedGame && (
              <button
                onClick={handleResumeGame}
                className="rounded-lg bg-amber-600 px-12 py-4 text-xl font-semibold transition-colors hover:bg-amber-500"
              >
                Resume Game
              </button>
            )}

            <button
              onClick={() => setMenuView("deck-gallery-ai")}
              className="rounded-lg bg-indigo-600 px-12 py-4 text-xl font-semibold transition-colors hover:bg-indigo-500"
            >
              {hasSavedGame ? "New Game vs AI" : "Play vs AI"}
            </button>

            <button
              onClick={() => setMenuView("deck-gallery-online")}
              className="rounded-lg bg-emerald-600 px-12 py-4 text-xl font-semibold transition-colors hover:bg-emerald-500"
            >
              Play Online
            </button>

            <button
              onClick={() => navigate("/deck-builder")}
              className="rounded-lg border border-gray-600 px-12 py-4 text-xl font-semibold transition-colors hover:border-gray-400"
            >
              Deck Builder
            </button>
          </div>

          <button
            onClick={() => setShowCoverage(true)}
            className="text-sm text-gray-500 transition-colors hover:text-gray-300"
          >
            Card Coverage
          </button>
        </div>
      )}

      {menuView === "deck-gallery-ai" && (
        <div className="relative z-10 flex w-full justify-center py-8">
          <DeckGallery
            onSelectDeck={handleSelectDeck}
            activeDeckName={activeDeckName}
            mode="ai"
            difficulty={difficulty}
            onDifficultyChange={setDifficulty}
            onStartGame={handleStartAIGame}
            onBack={() => setMenuView("mode-select")}
          />
        </div>
      )}

      {menuView === "deck-gallery-online" && (
        <div className="relative z-10 flex w-full justify-center py-8">
          <DeckGallery
            onSelectDeck={handleSelectDeck}
            activeDeckName={activeDeckName}
            mode="online"
            difficulty={difficulty}
            onDifficultyChange={setDifficulty}
            onStartGame={() => setMenuView("online-host-join")}
            onBack={() => setMenuView("mode-select")}
          />
        </div>
      )}

      {menuView === "online-host-join" && (
        <div className="relative z-10 flex flex-col items-center gap-4">
          <p className="text-lg font-medium text-gray-300">Multiplayer</p>

          <button
            onClick={handleHostOnlineGame}
            className="rounded-lg bg-emerald-600 px-8 py-3 text-base font-semibold transition-colors hover:bg-emerald-500"
          >
            Host Game
          </button>

          <button
            onClick={() => setMenuView("join-code")}
            className="rounded-lg bg-cyan-600 px-8 py-3 text-base font-semibold transition-colors hover:bg-cyan-500"
          >
            Join Game
          </button>

          <button
            onClick={() => setMenuView("deck-gallery-online")}
            className="mt-1 text-sm text-gray-400 hover:text-gray-200"
          >
            Back
          </button>
        </div>
      )}

      {menuView === "join-code" && (
        <div className="relative z-10 flex flex-col items-center gap-3">
          <p className="text-sm font-medium text-gray-300">Enter Game Code</p>

          <input
            type="text"
            value={joinCode}
            onChange={(e) => setJoinCode(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && handleJoinSubmit()}
            placeholder="e.g. ABC123"
            maxLength={10}
            className="w-48 rounded-lg bg-gray-800 px-4 py-2 text-center font-mono text-lg tracking-widest text-white placeholder-gray-500 outline-none ring-1 ring-gray-700 focus:ring-cyan-500"
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
            onClick={() => setMenuView("online-host-join")}
            className="mt-1 text-sm text-gray-400 hover:text-gray-200"
          >
            Back
          </button>
        </div>
      )}

      {showCoverage && (
        <CardCoverageDashboard onClose={() => setShowCoverage(false)} />
      )}
    </div>
  );
}
