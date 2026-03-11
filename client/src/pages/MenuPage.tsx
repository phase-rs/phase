import { useCallback, useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router";

import { initAudioOnInteraction } from "../audio/AudioManager";
import { ScreenChrome } from "../components/chrome/ScreenChrome";
import { CardCoverageDashboard } from "../components/controls/CardCoverageDashboard";
import { HostSetup } from "../components/lobby/HostSetup";
import { LobbyView } from "../components/lobby/LobbyView";
import { WaitingScreen } from "../components/lobby/WaitingScreen";
import { DeckGallery } from "../components/menu/DeckGallery";
import { MenuLogo } from "../components/menu/MenuLogo";
import { MenuParticles } from "../components/menu/MenuParticles";
import { menuButtonClass } from "../components/menu/buttonStyles";
import { ACTIVE_DECK_KEY, STORAGE_KEY_PREFIX } from "../constants/storage";
import { STARTER_DECKS } from "../data/starterDecks";
import { parseRoomCode } from "../network/connection";
import type { ParsedDeck } from "../services/deckParser";
import { useMultiplayerStore } from "../stores/multiplayerStore";
import {
  loadActiveGame,
  clearActiveGame,
  clearGame,
  saveActiveGame,
  useGameStore,
} from "../stores/gameStore";
import type { ActiveGameMeta } from "../stores/gameStore";
import type { HostSettings } from "../components/lobby/HostSetup";

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

function loadActiveDeck(): ParsedDeck | null {
  const activeName = localStorage.getItem(ACTIVE_DECK_KEY);
  if (!activeName) return null;
  const raw = localStorage.getItem(STORAGE_KEY_PREFIX + activeName);
  if (!raw) return null;
  return JSON.parse(raw) as ParsedDeck;
}

type MenuView =
  | "mode-select"
  | "deck-gallery-ai"
  | "deck-gallery-online"
  | "lobby"
  | "host-setup"
  | "waiting"
  | "join-code";

const BACK_TARGETS: Partial<Record<MenuView, MenuView>> = {
  "deck-gallery-ai": "mode-select",
  "deck-gallery-online": "mode-select",
  "lobby": "deck-gallery-online",
  "host-setup": "lobby",
  "waiting": "lobby",
  "join-code": "lobby",
};

export function MenuPage() {
  const navigate = useNavigate();
  const [showCoverage, setShowCoverage] = useState(false);
  const [menuView, setMenuView] = useState<MenuView>("mode-select");
  const [activeDeckName, setActiveDeckName] = useState<string | null>(null);
  const [activeGame, setActiveGame] = useState<ActiveGameMeta | null>(null);
  const [difficulty, setDifficulty] = useState("Medium");

  // Host/waiting state
  const [hostGameCode, setHostGameCode] = useState<string | null>(null);
  const [hostIsPublic, setHostIsPublic] = useState(true);
  const hostWsRef = useRef<WebSocket | null>(null);

  const serverAddress = useMultiplayerStore((s) => s.serverAddress);

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

  const handleResumeGame = () => {
    if (!activeGame) return;
    useGameStore.setState({ gameId: activeGame.id });
    navigate(`/game/${activeGame.id}?mode=${activeGame.mode}&difficulty=${activeGame.difficulty}`);
  };

  const handleHostWithSettings = useCallback(
    (settings: HostSettings) => {
      if (!activeDeckName) {
        setMenuView("deck-gallery-online");
        return;
      }

      sessionStorage.removeItem("phase-ws-session");
      setHostIsPublic(settings.public);

      const deck = loadActiveDeck();
      if (!deck) {
        setMenuView("deck-gallery-online");
        return;
      }

      // Build deck data
      const mainDeck: string[] = [];
      for (const entry of deck.main) {
        for (let i = 0; i < entry.count; i++) {
          mainDeck.push(entry.name);
        }
      }
      const sideboard: string[] = [];
      for (const entry of deck.sideboard) {
        for (let i = 0; i < entry.count; i++) {
          sideboard.push(entry.name);
        }
      }

      const ws = new WebSocket(serverAddress);
      hostWsRef.current = ws;

      ws.onopen = () => {
        ws.send(
          JSON.stringify({
            type: "CreateGameWithSettings",
            data: {
              deck: { main_deck: mainDeck, sideboard },
              display_name: settings.displayName,
              public: settings.public,
              password: settings.password || null,
              timer_seconds: settings.timerSeconds,
            },
          }),
        );
      };

      ws.onmessage = (event) => {
        const msg = JSON.parse(event.data as string) as { type: string; data?: unknown };

        if (msg.type === "GameCreated") {
          const data = msg.data as { game_code: string; player_token: string };
          setHostGameCode(data.game_code);
          // Persist session for reconnect
          sessionStorage.setItem(
            "phase-ws-session",
            JSON.stringify({ gameCode: data.game_code, playerToken: data.player_token }),
          );
          setMenuView("waiting");
        } else if (msg.type === "GameStarted") {
          // Game is ready — navigate to game page
          const gameId = crypto.randomUUID();
          useGameStore.setState({ gameId });
          navigate(`/game/${gameId}?mode=host`);
        } else if (msg.type === "Error") {
          const data = msg.data as { message: string };
          console.error("Host error:", data.message);
        }
      };

      ws.onerror = () => {
        console.error("Failed to connect to server");
      };
    },
    [activeDeckName, serverAddress, navigate],
  );

  const handleHostP2P = useCallback(() => {
    if (!activeDeckName) {
      setMenuView("deck-gallery-online");
      return;
    }
    const gameId = crypto.randomUUID();
    useGameStore.setState({ gameId });
    navigate(`/game/${gameId}?mode=p2p-host`);
  }, [activeDeckName, navigate]);

  const handleJoinWithPassword = useCallback(
    (code: string, password?: string) => {
      if (!activeDeckName) {
        setMenuView("deck-gallery-online");
        return;
      }

      // Detect P2P codes (5-char unambiguous alphabet) vs server codes
      const p2pCode = parseRoomCode(code);
      if (p2pCode && code.trim().length === 5) {
        const gameId = crypto.randomUUID();
        useGameStore.setState({ gameId });
        navigate(`/game/${gameId}?mode=p2p-join&code=${p2pCode}`);
        return;
      }

      sessionStorage.removeItem("phase-ws-session");
      const gameId = crypto.randomUUID();
      useGameStore.setState({ gameId });
      const params = new URLSearchParams({ mode: "join", code });
      if (password) {
        params.set("password", password);
      }
      navigate(`/game/${gameId}?${params.toString()}`);
    },
    [activeDeckName, navigate],
  );

  const handleCancelHost = useCallback(() => {
    if (hostWsRef.current) {
      hostWsRef.current.close();
      hostWsRef.current = null;
    }
    setHostGameCode(null);
    sessionStorage.removeItem("phase-ws-session");
    setMenuView("lobby");
  }, []);

  const backTarget = BACK_TARGETS[menuView];
  const handleBack = backTarget
    ? () => {
        // Clean up host connection when navigating away from waiting
        if (menuView === "waiting") {
          handleCancelHost();
          return;
        }
        setMenuView(backTarget);
      }
    : undefined;
  const hasSavedGame = activeGame !== null;

  return (
    <div className="relative flex min-h-screen flex-col items-center justify-center">
      <MenuParticles />
      <ScreenChrome showLogo={menuView !== "mode-select"} onBack={handleBack} />

      {menuView === "mode-select" && (
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
              onClick={() => setMenuView("deck-gallery-ai")}
              className={menuButtonClass({ tone: "indigo", size: "lg" })}
            >
              {hasSavedGame ? "New Game vs AI" : "Play vs AI"}
            </button>

            <button
              onClick={() => setMenuView("deck-gallery-online")}
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
            onStartGame={() => setMenuView("lobby")}
          />
        </div>
      )}

      {menuView === "lobby" && (
        <LobbyView
          onHostGame={() => setMenuView("host-setup")}
          onHostP2P={handleHostP2P}
          onJoinGame={handleJoinWithPassword}
          activeDeckName={activeDeckName}
          onChangeDeck={() => setMenuView("deck-gallery-online")}
        />
      )}

      {menuView === "host-setup" && (
        <HostSetup
          onHost={handleHostWithSettings}
          onBack={() => setMenuView("lobby")}
        />
      )}

      {menuView === "waiting" && hostGameCode && (
        <WaitingScreen
          gameCode={hostGameCode}
          isPublic={hostIsPublic}
          onCancel={handleCancelHost}
        />
      )}

      {showCoverage && (
        <CardCoverageDashboard onClose={() => setShowCoverage(false)} />
      )}
    </div>
  );
}
