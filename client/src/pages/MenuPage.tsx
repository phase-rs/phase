import { useEffect, useState } from "react";
import { useNavigate } from "react-router";

import { initAudioOnInteraction } from "../audio/AudioManager";
import { ScreenChrome } from "../components/chrome/ScreenChrome";
import { CardCoverageDashboard } from "../components/controls/CardCoverageDashboard";
import { MenuLogo } from "../components/menu/MenuLogo";
import { MenuParticles } from "../components/menu/MenuParticles";
import { menuButtonClass } from "../components/menu/buttonStyles";
import { listSavedDeckNames, STORAGE_KEY_PREFIX } from "../constants/storage";
import { STARTER_DECKS } from "../data/starterDecks";
import type { ParsedDeck } from "../services/deckParser";
import {
  clearActiveGame,
  loadActiveGame,
  loadGame,
  useGameStore,
} from "../stores/gameStore";
import type { ActiveGameMeta } from "../stores/gameStore";

function seedStarterDecks(): void {
  for (const starter of STARTER_DECKS) {
    const deck: ParsedDeck = { main: starter.cards, sideboard: [] };
    localStorage.setItem(STORAGE_KEY_PREFIX + starter.name, JSON.stringify(deck));
  }
}

export function MenuPage() {
  const navigate = useNavigate();
  const [showCoverage, setShowCoverage] = useState(false);
  const [activeGame, setActiveGame] = useState<ActiveGameMeta | null>(null);

  useEffect(() => {
    initAudioOnInteraction();
  }, []);

  useEffect(() => {
    const names = listSavedDeckNames();
    if (names.length === 0) {
      seedStarterDecks();
    }

    const saved = loadActiveGame();
    if (saved && loadGame(saved.id)) {
      setActiveGame(saved);
    } else if (saved) {
      // Metadata exists but game state is gone — clean up stale entry
      clearActiveGame();
    }
  }, []);

  const handleResumeGame = () => {
    if (!activeGame) return;
    useGameStore.setState({ gameId: activeGame.id });
    navigate(`/game/${activeGame.id}?mode=${activeGame.mode}&difficulty=${activeGame.difficulty}`);
  };

  const hasSavedGame = activeGame !== null;

  return (
    <div className="relative flex min-h-screen flex-col items-center justify-center">
      <MenuParticles />
      <ScreenChrome />

      <div className="relative z-10 flex flex-col items-center gap-8">
        <MenuLogo />
        <p className="text-gray-400">Magic: The Gathering Engine</p>

        <div className="flex flex-col gap-4">
          {hasSavedGame && (
            <button
              onClick={handleResumeGame}
              className={menuButtonClass({ tone: "amber", size: "lg" })}
            >
              Resume Game
            </button>
          )}

          <button
            onClick={() => navigate("/play")}
            className={menuButtonClass({ tone: "indigo", size: "lg" })}
          >
            {hasSavedGame ? "New Game vs AI" : "Play vs AI"}
          </button>

          <button
            onClick={() => navigate("/multiplayer")}
            className={menuButtonClass({ tone: "emerald", size: "lg" })}
          >
            Play Online
          </button>

          <button
            onClick={() => navigate("/deck-builder")}
            className={menuButtonClass({ tone: "neutral", size: "lg" })}
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

      {showCoverage && (
        <CardCoverageDashboard onClose={() => setShowCoverage(false)} />
      )}
    </div>
  );
}
