import { useCallback, useEffect, useRef, useState } from "react";

import type { GameFormat } from "../../adapter/types";
import { parseJoinCode } from "../../services/serverDetection";
import { useMultiplayerStore } from "../../stores/multiplayerStore";
import { MenuPanel } from "../menu/MenuShell";
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

  const handlePasswordSubmit = useCallback((e: React.FormEvent) => {
    e.preventDefault();
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
    <MenuPanel className="relative z-10 flex w-full max-w-xl flex-col gap-6 px-4 py-5">
      <div className="flex w-full items-center justify-between gap-3">
        <div className="text-[0.68rem] uppercase tracking-[0.22em] text-slate-500">
          {isP2P ? "Direct Connection" : "Online Lobby"}
        </div>
        {isServer && playerCount > 0 && (
          <span className="rounded-full bg-emerald-500/20 px-2.5 py-0.5 text-xs font-medium text-emerald-300">
            {playerCount} online
          </span>
        )}
      </div>

      <div className="flex w-full items-center justify-between gap-4 rounded-[18px] border border-white/10 bg-black/14 px-4 py-3">
        <div className="min-w-0">
          <div className="text-[0.68rem] uppercase tracking-[0.22em] text-slate-500">Selected Deck</div>
          <div className="mt-1 truncate text-sm font-medium text-white">
            {activeDeckName ?? "Choose a deck"}
          </div>
        </div>
        <button
          onClick={onChangeDeck}
          className={menuButtonClass({ tone: "neutral", size: "sm" })}
        >
          Change
        </button>
      </div>

      {isServer && (
        <div className="flex rounded-[16px] bg-black/18 p-0.5 ring-1 ring-white/10">
          {FORMAT_FILTERS.map((opt) => (
            <button
              key={opt.label}
              onClick={() => setFormatFilter(opt.value)}
              className={`rounded px-3 py-1 text-xs font-medium transition-colors ${
                formatFilter === opt.value
                  ? "bg-white/10 text-white"
                  : "text-gray-400 hover:text-gray-200"
              }`}
            >
              {opt.label}
            </button>
          ))}
        </div>
      )}

      {isServer && (
        <div className="w-full space-y-3">
          <div className="text-[0.68rem] uppercase tracking-[0.22em] text-slate-500">Open Tables</div>
          {filteredGames.length === 0 ? (
            <div className="rounded-[18px] border border-dashed border-white/10 py-8 text-center">
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

      {isP2P && (
        <div className="w-full rounded-[18px] border border-cyan-400/20 bg-cyan-500/[0.07] px-4 py-3 text-sm leading-6 text-cyan-100">
          Dedicated server unavailable. You can still host or join directly with a 5-character room code.
        </div>
      )}

      <div className="w-full space-y-3">
        <div className="text-[0.68rem] uppercase tracking-[0.22em] text-slate-500">
          {isP2P ? "Join by Code" : "Join a Table"}
        </div>
        <div className="flex w-full items-center gap-2">
        <input
          type="text"
          value={joinCode}
          onChange={(e) => setJoinCode(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && handleJoinByCode()}
          placeholder={isP2P ? "Enter 5-character P2P code" : "Enter code or CODE@IP:PORT"}
          maxLength={isP2P ? 5 : 50}
          className="flex-1 rounded-[18px] bg-black/18 px-4 py-2 font-mono text-sm tracking-wider text-white placeholder-gray-500 outline-none ring-1 ring-white/10 focus:ring-white/20"
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
      </div>

      <div className="flex w-full flex-col gap-3 border-t border-white/8 pt-4 sm:flex-row sm:items-center sm:justify-between">
        <div className="min-w-0">
          <div className="text-[0.68rem] uppercase tracking-[0.22em] text-slate-500">Host</div>
          <div className="mt-1 text-sm text-slate-400">
            {isP2P ? "Create a direct room for one opponent." : "Open a room and wait for players."}
          </div>
        </div>
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
          <div className="relative z-10 w-full max-w-xs rounded-[22px] border border-white/10 bg-[#0b1020]/96 p-6 shadow-2xl backdrop-blur-md">
            <h3 className="mb-3 text-sm font-semibold text-white">
              Password Required
            </h3>
            <form onSubmit={handlePasswordSubmit}>
              <input
                type="password"
                value={passwordInput}
                onChange={(e) => setPasswordInput(e.target.value)}
                placeholder="Enter password"
                className="mb-4 w-full rounded-lg bg-gray-800 px-3 py-2 text-sm text-white placeholder-gray-500 outline-none ring-1 ring-gray-700 focus:ring-cyan-500"
                autoFocus
              />
              <div className="flex justify-end gap-2">
                <button
                  type="button"
                  onClick={() => setPasswordModal(null)}
                  className={menuButtonClass({ tone: "neutral", size: "sm" })}
                >
                  Cancel
                </button>
                <button
                  type="submit"
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
            </form>
          </div>
        </div>
      )}
    </MenuPanel>
  );
}
