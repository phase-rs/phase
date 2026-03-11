import { useCallback, useEffect, useRef, useState } from "react";

import { useMultiplayerStore } from "../../stores/multiplayerStore";
import { menuButtonClass } from "../menu/buttonStyles";
import { GameListItem } from "./GameListItem";
import type { LobbyGame } from "./GameListItem";

interface LobbyViewProps {
  onHostGame: () => void;
  onJoinGame: (code: string, password?: string) => void;
  activeDeckName: string | null;
  onChangeDeck: () => void;
}

export function LobbyView({
  onHostGame,
  onJoinGame,
  activeDeckName,
  onChangeDeck,
}: LobbyViewProps) {
  const serverAddress = useMultiplayerStore((s) => s.serverAddress);
  const [games, setGames] = useState<LobbyGame[]>([]);
  const [playerCount, setPlayerCount] = useState(0);
  const [joinCode, setJoinCode] = useState("");
  const [passwordModal, setPasswordModal] = useState<{ gameCode: string } | null>(null);
  const [passwordInput, setPasswordInput] = useState("");
  const wsRef = useRef<WebSocket | null>(null);

  useEffect(() => {
    // Convert ws:// URL to connect for lobby subscription
    const ws = new WebSocket(serverAddress);
    wsRef.current = ws;

    ws.onopen = () => {
      ws.send(JSON.stringify({ type: "SubscribeLobby" }));
    };

    ws.onmessage = (event) => {
      const msg = JSON.parse(event.data as string) as { type: string; data?: unknown };

      switch (msg.type) {
        case "LobbyUpdate": {
          const data = msg.data as { games: LobbyGame[] };
          setGames(data.games);
          break;
        }
        case "LobbyGameAdded": {
          const data = msg.data as { game: LobbyGame };
          setGames((prev) => [...prev, data.game]);
          break;
        }
        case "LobbyGameRemoved": {
          const data = msg.data as { game_code: string };
          setGames((prev) => prev.filter((g) => g.game_code !== data.game_code));
          break;
        }
        case "PlayerCount": {
          const data = msg.data as { count: number };
          setPlayerCount(data.count);
          break;
        }
        case "PasswordRequired": {
          const data = msg.data as { game_code: string };
          setPasswordModal({ gameCode: data.game_code });
          setPasswordInput("");
          break;
        }
      }
    };

    ws.onerror = () => {
      // Connection failed — games list stays empty
    };

    return () => {
      if (ws.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify({ type: "UnsubscribeLobby" }));
      }
      ws.close();
      wsRef.current = null;
    };
  }, [serverAddress]);

  const handleJoinFromList = useCallback(
    (code: string) => {
      onJoinGame(code);
    },
    [onJoinGame],
  );

  const handleJoinByCode = useCallback(() => {
    const code = joinCode.trim().toUpperCase();
    if (code) {
      onJoinGame(code);
    }
  }, [joinCode, onJoinGame]);

  const handlePasswordSubmit = useCallback(() => {
    if (passwordModal && passwordInput) {
      onJoinGame(passwordModal.gameCode, passwordInput);
      setPasswordModal(null);
      setPasswordInput("");
    }
  }, [passwordModal, passwordInput, onJoinGame]);

  return (
    <div className="relative z-10 flex w-full max-w-lg flex-col items-center gap-6 px-4">
      {/* Header */}
      <div className="flex items-center gap-3">
        <h2 className="text-2xl font-bold tracking-tight text-white">Game Lobby</h2>
        {playerCount > 0 && (
          <span className="rounded-full bg-emerald-500/20 px-2.5 py-0.5 text-xs font-medium text-emerald-300">
            {playerCount} online
          </span>
        )}
      </div>

      {/* Current deck */}
      <div className="flex items-center gap-2 text-sm">
        <span className="text-gray-400">Deck:</span>
        <span className="font-medium text-gray-200">
          {activeDeckName ?? "No deck selected"}
        </span>
        <button
          onClick={onChangeDeck}
          className="text-cyan-400 transition-colors hover:text-cyan-300"
        >
          Change
        </button>
      </div>

      {/* Game list */}
      <div className="w-full">
        {games.length === 0 ? (
          <div className="rounded-lg border border-dashed border-gray-700 py-8 text-center">
            <p className="text-sm text-gray-500">
              No games available. Host one or join by code!
            </p>
          </div>
        ) : (
          <div className="flex max-h-64 flex-col gap-2 overflow-y-auto">
            {games.map((game) => (
              <GameListItem
                key={game.game_code}
                game={game}
                onJoin={handleJoinFromList}
              />
            ))}
          </div>
        )}
      </div>

      {/* Manual code entry */}
      <div className="flex w-full items-center gap-2">
        <input
          type="text"
          value={joinCode}
          onChange={(e) => setJoinCode(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && handleJoinByCode()}
          placeholder="Enter game code"
          maxLength={10}
          className="flex-1 rounded-lg bg-gray-800 px-4 py-2 font-mono text-sm tracking-wider text-white placeholder-gray-500 outline-none ring-1 ring-gray-700 focus:ring-cyan-500"
        />
        <button
          onClick={handleJoinByCode}
          disabled={!joinCode.trim()}
          className={menuButtonClass({
            tone: "cyan",
            size: "sm",
            disabled: !joinCode.trim(),
          })}
        >
          Join
        </button>
      </div>

      {/* Host Game button */}
      <button
        onClick={onHostGame}
        className={menuButtonClass({ tone: "emerald", size: "md" })}
      >
        Host Game
      </button>

      {/* Password modal */}
      {passwordModal && (
        <div className="fixed inset-0 z-50 flex items-center justify-center">
          <div
            className="absolute inset-0 bg-black/60"
            onClick={() => setPasswordModal(null)}
          />
          <div className="relative z-10 w-full max-w-xs rounded-xl bg-gray-900 p-6 shadow-2xl ring-1 ring-gray-700">
            <h3 className="mb-3 text-sm font-semibold text-white">
              Password Required
            </h3>
            <input
              type="password"
              value={passwordInput}
              onChange={(e) => setPasswordInput(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && handlePasswordSubmit()}
              placeholder="Enter password"
              className="mb-4 w-full rounded-lg bg-gray-800 px-3 py-2 text-sm text-white placeholder-gray-500 outline-none ring-1 ring-gray-700 focus:ring-cyan-500"
              autoFocus
            />
            <div className="flex justify-end gap-2">
              <button
                onClick={() => setPasswordModal(null)}
                className={menuButtonClass({ tone: "neutral", size: "sm" })}
              >
                Cancel
              </button>
              <button
                onClick={handlePasswordSubmit}
                disabled={!passwordInput}
                className={menuButtonClass({
                  tone: "cyan",
                  size: "sm",
                  disabled: !passwordInput,
                })}
              >
                Join
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
