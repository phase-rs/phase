import { useCallback, useEffect, useMemo, useState } from "react";
import { useNavigate } from "react-router";

import { initAudioOnInteraction } from "../audio/AudioManager";
import { ScreenChrome } from "../components/chrome/ScreenChrome";
import { AiDifficultyDropdown } from "../components/menu/AiDifficultyDropdown";
import { MainMenuActionCard } from "../components/menu/MainMenuActionCard";
import { MenuLogo } from "../components/menu/MenuLogo";
import { MenuParticles } from "../components/menu/MenuParticles";
import { getAiDifficultyLabel } from "../constants/ai";
import {
  ACTIVE_DECK_KEY,
  listSavedDeckNames,
  STORAGE_KEY_PREFIX,
} from "../constants/storage";
import { STARTER_DECKS } from "../data/starterDecks";
import type { ParsedDeck } from "../services/deckParser";
import {
  clearActiveGame,
  loadActiveGame,
  loadGame,
  useGameStore,
} from "../stores/gameStore";
import type { ActiveGameMeta } from "../stores/gameStore";
import { usePreferencesStore } from "../stores/preferencesStore";

function seedStarterDecks(): void {
  for (const starter of STARTER_DECKS) {
    const deck: ParsedDeck = { main: starter.cards, sideboard: [] };
    localStorage.setItem(STORAGE_KEY_PREFIX + starter.name, JSON.stringify(deck));
  }
}

export function MenuPage() {
  const navigate = useNavigate();
  const [activeGame, setActiveGame] = useState<ActiveGameMeta | null>(null);
  const [, setDeckCount] = useState(0);
  const [, setActiveDeckName] = useState<string | null>(null);
  const aiDifficulty = usePreferencesStore((s) => s.aiDifficulty);
  const setAiDifficulty = usePreferencesStore((s) => s.setAiDifficulty);

  useEffect(() => {
    initAudioOnInteraction();
  }, []);

  useEffect(() => {
    const names = listSavedDeckNames();
    if (names.length === 0) {
      seedStarterDecks();
    }
    const savedNames = listSavedDeckNames();
    setDeckCount(savedNames.length);
    setActiveDeckName(localStorage.getItem(ACTIVE_DECK_KEY));

    const saved = loadActiveGame();
    if (saved) {
      const hasState = saved.mode === "online"
        ? sessionStorage.getItem("phase-ws-session") !== null
        : loadGame(saved.id) !== null;
      if (hasState) {
        setActiveGame(saved);
      } else {
        // Metadata exists but game state is gone — clean up stale entry
        clearActiveGame();
      }
    }
  }, []);

  const handleResumeGame = useCallback(() => {
    if (!activeGame) return;
    useGameStore.setState({ gameId: activeGame.id });
    if (activeGame.mode === "online") {
      // Reconnect via session token
      navigate(`/game/${activeGame.id}?mode=host`);
    } else {
      navigate(`/game/${activeGame.id}?mode=${activeGame.mode}&difficulty=${activeGame.difficulty}`);
    }
  }, [activeGame, navigate]);

  const hasSavedGame = activeGame !== null;
  const menuActions = useMemo(() => {
    const actions = [];
    if (hasSavedGame) {
      actions.push({
        key: "resume",
        title: "Resume Game",
        description: "Continue the last saved match from its current turn and board state.",
        accent: "ember" as const,
        onClick: handleResumeGame,
        icon: <ResumeIcon />,
      });
    }
    actions.push(
      {
        key: "setup",
        title: hasSavedGame ? "Start New Match" : "Start Match",
        description: "Choose format, rules, and deck before starting a new match.",
        accent: "arcane" as const,
        onClick: () => navigate("/setup"),
        icon: <SigilIcon />,
      },
      {
        key: "quick",
        title: "Quick Duel vs AI",
        description: `Start a one-on-one game with your selected deck. Current AI: ${getAiDifficultyLabel(aiDifficulty)}.`,
        accent: "stone" as const,
        onClick: () => navigate("/play"),
        icon: <SparkIcon />,
        aside: (
          <AiDifficultyDropdown
            difficulty={aiDifficulty}
            onChange={setAiDifficulty}
            compact
            className="h-full"
          />
        ),
      },
      {
        key: "online",
        title: "Play Online",
        description: "Host a room, join by code, or reconnect to multiplayer.",
        accent: "jade" as const,
        onClick: () => navigate("/multiplayer"),
        icon: <CrownIcon />,
      },
      {
        key: "decks",
        title: "Decks",
        description: "Open saved decks, switch your active list, and edit builds.",
        accent: "stone" as const,
        onClick: () => navigate("/my-decks"),
        icon: <DeckIcon />,
      },
    );
    return actions;
  }, [aiDifficulty, hasSavedGame, navigate, handleResumeGame, setAiDifficulty]);

  return (
    <div className="menu-scene relative flex min-h-screen flex-col overflow-hidden">
      <MenuParticles />
      <ScreenChrome />
      <div className="menu-scene__vignette" />
      <div className="menu-scene__sigil menu-scene__sigil--left" />
      <div className="menu-scene__sigil menu-scene__sigil--right" />
      <div className="menu-scene__haze" />

      <div className="relative z-10 mx-auto flex min-h-screen w-full max-w-7xl flex-col justify-center px-6 py-16 lg:px-10">
        <div className="mx-auto flex w-full max-w-3xl flex-col items-center text-center">
          <div>
            <MenuLogo />
          </div>
        </div>

        <div className="mx-auto mt-10 flex w-full max-w-3xl flex-col gap-2.5">
          {menuActions.map((action) => (
            <MainMenuActionCard
              key={action.key}
              title={action.title}
              description={action.description}
              accent={action.accent}
              onClick={action.onClick}
              icon={action.icon}
              aside={action.aside}
            />
          ))}
        </div>

        <div className="mt-6 flex flex-wrap items-center justify-center gap-3">
          <button
            onClick={() => navigate("/coverage")}
            className="inline-flex items-center rounded-full border border-white/10 bg-black/20 px-4 py-2 text-sm font-medium text-slate-400 transition-colors hover:border-white/20 hover:text-white"
          >
            View Card Coverage
          </button>
          {hasSavedGame && (
            <div className="rounded-full border border-white/8 bg-black/16 px-4 py-2 text-sm text-slate-500">
              Saved match available
            </div>
          )}
        </div>
      </div>

    </div>
  );
}

function ResumeIcon() {
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24" className="h-7 w-7 fill-current">
      <path d="M12 3a9 9 0 1 0 8.95 10h-2.07A7 7 0 1 1 12 5a6.96 6.96 0 0 1 4.95 2.05L14 10h7V3l-2.64 2.64A8.95 8.95 0 0 0 12 3Z" />
    </svg>
  );
}

function SigilIcon() {
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24" className="h-7 w-7 fill-current">
      <path d="M12 2 4 6v6c0 5.2 3.4 9.8 8 11 4.6-1.2 8-5.8 8-11V6l-8-4Zm0 5.2 2 4.05 4.5.65-3.25 3.16.77 4.47L12 17.34 7.98 19.5l.77-4.47L5.5 11.9l4.5-.65L12 7.2Z" />
    </svg>
  );
}

function SparkIcon() {
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24" className="h-7 w-7 fill-current">
      <path d="m13 2-1.9 7H6l5 4-1.9 9L18 11h-5l2-9Z" />
    </svg>
  );
}

function CrownIcon() {
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24" className="h-7 w-7 fill-current">
      <path d="m3 18 1.9-9 4.35 3.76L12 6l2.75 6.76L19.1 9 21 18H3Zm1 2h16v2H4v-2Z" />
    </svg>
  );
}

function DeckIcon() {
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24" className="h-7 w-7 fill-current">
      <path d="M7 3h9a2 2 0 0 1 2 2v11a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2Zm1 3v9h7V6H8Zm-2 15h11v-2H6v2Z" />
    </svg>
  );
}
