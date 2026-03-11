import { useEffect, useState } from "react";
import { useNavigate } from "react-router";

import { ScreenChrome } from "../components/chrome/ScreenChrome";
import { DeckGallery } from "../components/menu/DeckGallery";
import { MenuParticles } from "../components/menu/MenuParticles";
import { ACTIVE_DECK_KEY, STORAGE_KEY_PREFIX, listSavedDeckNames } from "../constants/storage";
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

function seedStarterDecks(): void {
  for (const starter of STARTER_DECKS) {
    const deck: ParsedDeck = { main: starter.cards, sideboard: [] };
    localStorage.setItem(STORAGE_KEY_PREFIX + starter.name, JSON.stringify(deck));
  }
}

export function PlayPage() {
  const navigate = useNavigate();
  const [activeDeckName, setActiveDeckName] = useState<string | null>(null);
  const [activeGame, setActiveGame] = useState<ActiveGameMeta | null>(null);
  const [difficulty, setDifficulty] = useState("Medium");

  useEffect(() => {
    const names = listSavedDeckNames();
    if (names.length === 0) {
      seedStarterDecks();
    }
    setActiveDeckName(localStorage.getItem(ACTIVE_DECK_KEY));
    setActiveGame(loadActiveGame());
  }, []);

  const handleSelectDeck = (name: string) => {
    setActiveDeckName(name);
    localStorage.setItem(ACTIVE_DECK_KEY, name);
  };

  const handleStartGame = () => {
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

  return (
    <div className="relative flex min-h-screen flex-col items-center justify-center">
      <MenuParticles />
      <ScreenChrome onBack={() => navigate("/")} />

      <div className="relative z-10 flex w-full justify-center py-8">
        <DeckGallery
          onSelectDeck={handleSelectDeck}
          activeDeckName={activeDeckName}
          mode="ai"
          difficulty={difficulty}
          onDifficultyChange={setDifficulty}
          onStartGame={handleStartGame}
        />
      </div>
    </div>
  );
}
