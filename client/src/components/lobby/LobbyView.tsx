import { useCallback, useEffect, useRef, useState } from "react";

import type { GameFormat } from "../../adapter/types";
import { parseJoinCode } from "../../services/serverDetection";
import { useMultiplayerStore } from "../../stores/multiplayerStore";
import { menuButtonClass } from "../menu/buttonStyles";
import { GameListItem } from "./GameListItem";
import type { LobbyGame } from "./GameListItem";

interface LobbyViewProps {
  onHostGame: () => void;
  onHostP2P: () => void;
  onJoinGame: (code: string, password?: string) => void;
  activeDeckName: string | null;
  onChangeDeck: () => void;
  connectionMode?: "server" | "p2p";
  onServerOffline?: () => void;
}

const FORMAT_FILTERS: { value: GameFormat | null; label: string }[] = [
  { value: null, label: "All" },
  { value: "Standard", label: "STD" },
  { value: "Commander", label: "CMD" },
  { value: "FreeForAll", label: "FFA" },
  { value: "TwoHeadedGiant", label: "2HG" },
];

export function LobbyView({
  onHostGame,
  onHostP2P,
  onJoinGame,
  activeDeckName,
  onChangeDeck,
  connectionMode,
  onServerOffline,
}: LobbyViewProps) {
  const isServer = connectionMode !== "p2p";
  const isP2P = connectionMode === "p2p";
  const serverAddress = useMultiplayerStore((s) => s.serverAddress);
  const [games, setGames] = useState<LobbyGame[]>([]);
  const [playerCount, setPlayerCount] = useState(0);
  const [joinCode, setJoinCode] = useState("");
  const [passwordModal, setPasswordModal] = useState<{ gameCode: string } | null>(null);
  const [passwordInput, setPasswordInput] = useState("");
  const [formatFilter, setFormatFilter] = useState<GameFormat | null>(null);
  const wsRef = useRef<WebSocket | null>(null);

  useEffect(() => {
    // P2P mode doesn't need server lobby connection
    if (isP2P) return;

    let connected = false;
    let isCleaningUp = false;
    let notifiedOffline = false;
    const notifyServerOffline = () => {
      if (connected || isCleaningUp || notifiedOffline) {
        return;
      }
      notifiedOffline = true;
      onServerOffline?.();
    };

    // Connect to server lobby for game list subscription
    const ws = new WebSocket(serverAddress);
    wsRef.current = ws;

    ws.onopen = () => {
      connected = true;
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
      notifyServerOffline();
    };

    ws.onclose = () => {
      notifyServerOffline();
    };

    return () => {
      isCleaningUp = true;
      if (ws.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify({ type: "UnsubscribeLobby" }));
      }
      ws.close();
      wsRef.current = null;
    };
  }, [serverAddress, isP2P, onServerOffline]);

  const handleJoinFromList = useCallback(
    (code: string) => {
      onJoinGame(code);
    },
    [onJoinGame],
  );

  const handleJoinByCode = useCallback(() => {
    const raw = joinCode.trim().toUpperCase();
    if (!raw) return;

    const parsed = parseJoinCode(raw);
    if (parsed.serverAddress) {
      // CODE@IP:PORT format -- update server address and join
      useMultiplayerStore.getState().setServerAddress(parsed.serverAddress);
    }
    onJoinGame(parsed.code);
  }, [joinCode, onJoinGame]);

  const handlePasswordSubmit = useCallback(() => {
    if (passwordModal && passwordInput) {
      onJoinGame(passwordModal.gameCode, passwordInput);
      setPasswordModal(null);
      setPasswordInput("");
    }
  }, [passwordModal, passwordInput, onJoinGame]);

  const filteredGames = formatFilter
    ? games.filter((g) => (g.format ?? "Standard") === formatFilter)
    : games;

  return (
    <div className="relative z-10 flex w-full max-w-lg flex-col items-center gap-6 px-4">
      {/* Header */}
      <div className="flex items-center gap-3">
        <h2 className="text-2xl font-bold tracking-tight text-white">
          {isP2P ? "Peer-to-Peer" : "Online Lobby"}
        </h2>
        {isServer && playerCount > 0 && (
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

      {/* Format filter — server only */}
      {isServer && (
        <div className="flex rounded bg-gray-800 p-0.5 ring-1 ring-gray-700">
          {FORMAT_FILTERS.map((opt) => (
            <button
              key={opt.label}
              onClick={() => setFormatFilter(opt.value)}
              className={`rounded px-3 py-1 text-xs font-medium transition-colors ${
                formatFilter === opt.value
                  ? "bg-cyan-600 text-white"
                  : "text-gray-400 hover:text-gray-200"
              }`}
            >
              {opt.label}
            </button>
          ))}
        </div>
      )}

      {/* Game list — server only */}
      {isServer && (
        <div className="w-full">
          {filteredGames.length === 0 ? (
            <div className="rounded-lg border border-dashed border-gray-700 py-8 text-center">
              <p className="text-sm text-gray-500">
                No games available. Host one or join by code!
              </p>
            </div>
          ) : (
            <div className="flex max-h-64 flex-col gap-2 overflow-y-auto">
              {filteredGames.map((game) => (
                <GameListItem
                  key={game.game_code}
                  game={game}
                  onJoin={handleJoinFromList}
                />
              ))}
            </div>
          )}
        </div>
      )}

      {/* P2P description */}
      {isP2P && (
        <div className="w-full rounded-lg border border-cyan-500/20 bg-cyan-500/5 px-4 py-3 text-sm text-cyan-200">
          Direct peer-to-peer connection. Host a game and share the 5-character code with your opponent.
        </div>
      )}

      {/* Manual code entry */}
      <div className="flex w-full items-center gap-2">
        <input
          type="text"
          value={joinCode}
          onChange={(e) => setJoinCode(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && handleJoinByCode()}
          placeholder={isP2P ? "Enter 5-character P2P code" : "Enter code or CODE@IP:PORT"}
          maxLength={isP2P ? 5 : 50}
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

      {/* Host Game button — mode-specific */}
      <div className="flex gap-3">
        {isServer && (
          <button
            onClick={onHostGame}
            className={menuButtonClass({ tone: "emerald", size: "md" })}
          >
            Host Game
          </button>
        )}
        {isP2P && (
          <button
            onClick={onHostP2P}
            className={menuButtonClass({ tone: "cyan", size: "md" })}
          >
            Host P2P Game
          </button>
        )}
      </div>

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
