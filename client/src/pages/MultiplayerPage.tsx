import { useCallback, useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router";

import { ScreenChrome } from "../components/chrome/ScreenChrome";
import { HostSetup } from "../components/lobby/HostSetup";
import { LobbyView } from "../components/lobby/LobbyView";
import { WaitingScreen } from "../components/lobby/WaitingScreen";
import { MenuParticles } from "../components/menu/MenuParticles";
import { MenuShell } from "../components/menu/MenuShell";
import { MyDecks } from "../components/menu/MyDecks";
import { ACTIVE_DECK_KEY, STORAGE_KEY_PREFIX, listSavedDeckNames, stampDeckMeta } from "../constants/storage";
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
    stampDeckMeta(starter.name, 0);
  }
}

function loadActiveDeck(): ParsedDeck | null {
  const activeName = localStorage.getItem(ACTIVE_DECK_KEY);
  if (!activeName) return null;
  const raw = localStorage.getItem(STORAGE_KEY_PREFIX + activeName);
  if (!raw) return null;
  return JSON.parse(raw) as ParsedDeck;
}

type ConnectionMode = "server" | "p2p";
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

  const [hostGameCode, setHostGameCode] = useState<string | null>(null);
  const [hostIsPublic, setHostIsPublic] = useState(true);
  const [connectionMode, setConnectionMode] = useState<ConnectionMode>("server");
  const [showSettings, setShowSettings] = useState(false);
  const hostWsRef = useRef<WebSocket | null>(null);

  const serverAddress = useMultiplayerStore((s) => s.serverAddress);

  const fallbackToP2PMode = useCallback(() => {
    setConnectionMode("p2p");
    setView("lobby");
  }, []);

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
              player_count: settings.formatConfig.max_players,
              match_config: { match_type: settings.matchType },
              format_config: settings.formatConfig,
              ai_seats: settings.aiSeats,
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
        ws.close();
        hostWsRef.current = null;
        fallbackToP2PMode();
      };
    },
    [activeDeckName, serverAddress, navigate, fallbackToP2PMode],
  );

  const handleHostP2P = useCallback((settings: HostSettings) => {
    if (!activeDeckName) {
      setView("deck-select");
      return;
    }
    const gameId = crypto.randomUUID();
    useGameStore.setState({ gameId });
    navigate(`/game/${gameId}?mode=p2p-host&match=${settings.matchType.toLowerCase()}`);
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

  const title = view === "deck-select"
    ? "Choose a deck for multiplayer."
    : view === "lobby"
      ? "Join or host a table."
      : view === "host-setup"
        ? "Set up your table."
        : "Waiting for players.";

  const description = view === "deck-select"
    ? "Pick the deck you want to bring online."
    : view === "lobby"
      ? "Browse available tables, join by code, or host a new match."
      : view === "host-setup"
        ? "Adjust format, privacy, and timing before opening the room."
        : "Share the code and wait for the room to fill.";

  return (
    <div className="menu-scene relative flex min-h-screen flex-col overflow-hidden">
      <MenuParticles />
      <ScreenChrome
        onBack={handleBack}
        settingsOpen={showSettings}
        onSettingsOpenChange={setShowSettings}
      />
      <div className="menu-scene__vignette" />
      <div className="menu-scene__sigil menu-scene__sigil--left" />
      <div className="menu-scene__sigil menu-scene__sigil--right" />
      <div className="menu-scene__haze" />

      <MenuShell eyebrow="Multiplayer" title={title} description={description} layout="stacked">
        {view === "deck-select" && (
          <MyDecks
            mode="select"
            onSelectDeck={handleSelectDeck}
            activeDeckName={activeDeckName}
            onConfirmSelection={() => setView("lobby")}
            confirmLabel="Continue"
          />
        )}

        {view === "lobby" && (
          <LobbyView
            onHostGame={() => { setConnectionMode("server"); setView("host-setup"); }}
            onHostP2P={() => { setConnectionMode("p2p"); setView("host-setup"); }}
            onJoinGame={handleJoinWithPassword}
            activeDeckName={activeDeckName}
            onChangeDeck={() => setView("deck-select")}
            connectionMode={connectionMode}
            onServerOffline={fallbackToP2PMode}
          />
        )}

        {view === "host-setup" && (
          <HostSetup
            onHost={connectionMode === "p2p" ? handleHostP2P : handleHostWithSettings}
            onBack={() => setView("lobby")}
            connectionMode={connectionMode}
          />
        )}

        {view === "waiting" && hostGameCode && (
          <WaitingScreen
            gameCode={hostGameCode}
            isPublic={hostIsPublic}
            onCancel={handleCancelHost}
          />
        )}
      </MenuShell>

    </div>
  );
}
