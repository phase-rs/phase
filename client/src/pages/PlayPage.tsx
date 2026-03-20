import { useEffect, useState } from "react";
import { useNavigate } from "react-router";

import { ScreenChrome } from "../components/chrome/ScreenChrome";
import { AiDifficultyDropdown } from "../components/menu/AiDifficultyDropdown";
import { MenuParticles } from "../components/menu/MenuParticles";
import { MenuShell } from "../components/menu/MenuShell";
import { MyDecks } from "../components/menu/MyDecks";
import { ACTIVE_DECK_KEY, STORAGE_KEY_PREFIX, listSavedDeckNames, stampDeckMeta } from "../constants/storage";
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
import { usePreferencesStore } from "../stores/preferencesStore";

function seedStarterDecks(): void {
  for (const starter of STARTER_DECKS) {
    const deck: ParsedDeck = { main: starter.cards, sideboard: [] };
    localStorage.setItem(STORAGE_KEY_PREFIX + starter.name, JSON.stringify(deck));
    stampDeckMeta(starter.name, 0);
  }
}

export function PlayPage() {
  const navigate = useNavigate();
  const [activeDeckName, setActiveDeckName] = useState<string | null>(null);
  const [activeGame, setActiveGame] = useState<ActiveGameMeta | null>(null);
  const difficulty = usePreferencesStore((s) => s.aiDifficulty);
  const setDifficulty = usePreferencesStore((s) => s.setAiDifficulty);

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
    navigate(`/game/${gameId}?mode=ai&difficulty=${difficulty}&match=bo1`);
  };

  return (
    <div className="menu-scene relative flex min-h-screen flex-col overflow-hidden">
      <MenuParticles />
      <ScreenChrome onBack={() => navigate("/")} />
      <div className="menu-scene__vignette" />
      <div className="menu-scene__sigil menu-scene__sigil--left" />
      <div className="menu-scene__sigil menu-scene__sigil--right" />
      <div className="menu-scene__haze" />

      <MenuShell
        eyebrow="Quick Duel"
        title="Quick duel."
        description="Choose a deck and start a one-on-one match."
        layout="stacked"
      >
        <MyDecks
          mode="select"
          onSelectDeck={handleSelectDeck}
          activeDeckName={activeDeckName}
          confirmAction={(
            <div className="flex overflow-hidden rounded-[14px] border border-indigo-300/18 shadow-[0_10px_28px_rgba(49,46,129,0.24)]">
              <button
                type="button"
                onClick={handleStartGame}
                disabled={!activeDeckName}
                className={[
                  "min-h-11 px-4 py-2 text-sm font-medium transition-colors",
                  activeDeckName
                    ? "bg-indigo-400/12 text-indigo-100 hover:bg-indigo-400/16"
                    : "bg-white/5 text-white/30",
                ].join(" ")}
              >
                Start Game
              </button>
              <div className="border-l border-indigo-300/18">
                <AiDifficultyDropdown
                  difficulty={difficulty}
                  onChange={setDifficulty}
                  compact
                  className="h-full"
                />
              </div>
            </div>
          )}
        />
      </MenuShell>
    </div>
  );
}
