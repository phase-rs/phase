import { useCallback, useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router";

import { ScreenChrome } from "../components/chrome/ScreenChrome";
import { HostSetup } from "../components/lobby/HostSetup";
import { LobbyView } from "../components/lobby/LobbyView";
import { WaitingScreen } from "../components/lobby/WaitingScreen";
import { DeckGallery } from "../components/menu/DeckGallery";
import { MenuParticles } from "../components/menu/MenuParticles";
import { ACTIVE_DECK_KEY, STORAGE_KEY_PREFIX, listSavedDeckNames } from "../constants/storage";
import { STARTER_DECKS } from "../data/starterDecks";
import { parseRoomCode } from "../network/connection";
import type { ParsedDeck } from "../services/deckParser";
import { useMultiplayerStore } from "../stores/multiplayerStore";
import { useGameStore, saveActiveGame } from "../stores/gameStore";
import type { HostSettings } from "../components/lobby/HostSetup";

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

type MultiplayerView = "deck-select" | "lobby" | "host-setup" | "waiting";

const BACK_TARGETS: Record<MultiplayerView, string> = {
  "deck-select": "/",
  "lobby": "deck-select",
  "host-setup": "lobby",
  "waiting": "lobby",
};

export function MultiplayerPage() {
  const navigate = useNavigate();
  const [view, setView] = useState<MultiplayerView>("deck-select");
  const [activeDeckName, setActiveDeckName] = useState<string | null>(null);
  const [difficulty, setDifficulty] = useState("Medium");

  const [hostGameCode, setHostGameCode] = useState<string | null>(null);
  const [hostIsPublic, setHostIsPublic] = useState(true);
  const hostWsRef = useRef<WebSocket | null>(null);

  const serverAddress = useMultiplayerStore((s) => s.serverAddress);

  useEffect(() => {
    const names = listSavedDeckNames();
    if (names.length === 0) {
      seedStarterDecks();
    }
    setActiveDeckName(localStorage.getItem(ACTIVE_DECK_KEY));
  }, []);

  const handleSelectDeck = (name: string) => {
    setActiveDeckName(name);
    localStorage.setItem(ACTIVE_DECK_KEY, name);
  };

  const handleHostWithSettings = useCallback(
    (settings: HostSettings) => {
      if (!activeDeckName) {
        setView("deck-select");
        return;
      }

      sessionStorage.removeItem("phase-ws-session");
      setHostIsPublic(settings.public);

      const deck = loadActiveDeck();
      if (!deck) {
        setView("deck-select");
        return;
      }

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
          sessionStorage.setItem(
            "phase-ws-session",
            JSON.stringify({ gameCode: data.game_code, playerToken: data.player_token }),
          );
          setView("waiting");
        } else if (msg.type === "GameStarted") {
          // Close this pre-game WS before navigating — GameProvider will
          // reconnect using the saved session token.
          ws.close();
          hostWsRef.current = null;
          const gameId = crypto.randomUUID();
          saveActiveGame({ id: gameId, mode: "online", difficulty: "" });
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
      setView("deck-select");
      return;
    }
    const gameId = crypto.randomUUID();
    useGameStore.setState({ gameId });
    navigate(`/game/${gameId}?mode=p2p-host`);
  }, [activeDeckName, navigate]);

  const handleJoinWithPassword = useCallback(
    (code: string, password?: string) => {
      if (!activeDeckName) {
        setView("deck-select");
        return;
      }

      const p2pCode = parseRoomCode(code);
      if (p2pCode && code.trim().length === 5) {
        const gameId = crypto.randomUUID();
        useGameStore.setState({ gameId });
        navigate(`/game/${gameId}?mode=p2p-join&code=${p2pCode}`);
        return;
      }

      sessionStorage.removeItem("phase-ws-session");
      const gameId = crypto.randomUUID();
      saveActiveGame({ id: gameId, mode: "online", difficulty: "" });
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
    setView("lobby");
  }, []);

  const handleBack = () => {
    if (view === "waiting") {
      handleCancelHost();
      return;
    }
    const target = BACK_TARGETS[view];
    if (target === "/") {
      navigate("/");
    } else {
      setView(target as MultiplayerView);
    }
  };

  return (
    <div className="relative flex min-h-screen flex-col items-center justify-center">
      <MenuParticles />
      <ScreenChrome onBack={handleBack} />

      {view === "deck-select" && (
        <div className="relative z-10 flex w-full justify-center py-8">
          <DeckGallery
            onSelectDeck={handleSelectDeck}
            activeDeckName={activeDeckName}
            mode="online"
            difficulty={difficulty}
            onDifficultyChange={setDifficulty}
            onStartGame={() => setView("lobby")}
          />
        </div>
      )}

      {view === "lobby" && (
        <LobbyView
          onHostGame={() => setView("host-setup")}
          onHostP2P={handleHostP2P}
          onJoinGame={handleJoinWithPassword}
          activeDeckName={activeDeckName}
          onChangeDeck={() => setView("deck-select")}
        />
      )}

      {view === "host-setup" && (
        <HostSetup
          onHost={handleHostWithSettings}
          onBack={() => setView("lobby")}
        />
      )}

      {view === "waiting" && hostGameCode && (
        <WaitingScreen
          gameCode={hostGameCode}
          isPublic={hostIsPublic}
          onCancel={handleCancelHost}
        />
      )}
    </div>
  );
}
