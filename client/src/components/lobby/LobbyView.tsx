import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import type { FormatGroup, GameFormat } from "../../adapter/types";
import { FORMAT_REGISTRY } from "../../data/formatRegistry";
import { parseJoinCode } from "../../services/serverDetection";
import { FORMAT_DEFAULTS, isLobbyEntryCompatible, useMultiplayerStore } from "../../stores/multiplayerStore";
import { MenuPanel } from "../menu/MenuShell";
import { menuButtonClass } from "../menu/buttonStyles";
import { GameListItem } from "./GameListItem";
import type { LobbyGame } from "./GameListItem";
import { ServerPicker } from "./ServerPicker";

interface LobbyViewProps {
  onHostGame: () => void;
  onHostP2P: () => void;
  /**
   * Called when the user elects to join a game. `context` is the full
   * `LobbyGame` row when the join originates from the lobby list, so
   * downstream views (e.g. the deck picker) can render "Joining Alice's
   * Commander game — 2/4". It is absent for typed-code joins.
   */
  onJoinGame: (
    code: string,
    password?: string,
    format?: GameFormat,
    context?: LobbyGame,
  ) => void;
  connectionMode?: "server" | "p2p";
  onServerOffline?: () => void;
}

// <optgroup> render order for the format filter <select>. New engine
// FormatGroup variants become a TS exhaustiveness error here.
const FILTER_GROUP_ORDER: Record<FormatGroup, number> = {
  Constructed: 0,
  Commander: 1,
  Multiplayer: 2,
};

const FORMAT_FILTER_GROUPS = (Object.keys(FILTER_GROUP_ORDER) as FormatGroup[])
  .sort((a, b) => FILTER_GROUP_ORDER[a] - FILTER_GROUP_ORDER[b])
  .map((group) => ({
    group,
    items: FORMAT_REGISTRY.filter((m) => m.group === group),
  }))
  .filter((g) => g.items.length > 0);

const FILTER_ALL_SENTINEL = "__all__";

type RoomTypeFilter = "all" | "p2p" | "server";

const ROOM_TYPE_FILTERS: { value: RoomTypeFilter; label: string }[] = [
  { value: "all", label: "All" },
  { value: "p2p", label: "P2P" },
  { value: "server", label: "Server" },
];

export function LobbyView({
  onHostGame,
  onHostP2P,
  onJoinGame,
  connectionMode,
  onServerOffline,
}: LobbyViewProps) {
  const isServer = connectionMode !== "p2p";
  const isP2P = connectionMode === "p2p";
  const serverAddress = useMultiplayerStore((s) => s.serverAddress);
  const [games, setGames] = useState<LobbyGame[]>([]);
  const gamesRef = useRef<LobbyGame[]>([]);
  const [playerCount, setPlayerCount] = useState(0);
  const [joinCode, setJoinCode] = useState("");
  const [passwordModal, setPasswordModal] = useState<{
    gameCode: string;
    format?: GameFormat;
    /** Full lobby row when click came from the list — propagates into
     * the join handler as deck-picker context. */
    context?: LobbyGame;
  } | null>(null);
  const [passwordInput, setPasswordInput] = useState("");
  const [formatFilter, setFormatFilter] = useState<GameFormat | null>(null);
  const [roomTypeFilter, setRoomTypeFilter] = useState<RoomTypeFilter>("all");
  const [serverPickerOpen, setServerPickerOpen] = useState(false);
  const subscribeLobby = useMultiplayerStore((s) => s.subscribeLobby);
  const ensureSubscriptionSocket = useMultiplayerStore(
    (s) => s.ensureSubscriptionSocket,
  );
  const setFormatConfig = useMultiplayerStore((s) => s.setFormatConfig);

  // If the user is browsing a specific format and clicks Host, seed the
  // host-setup form with that format — they were clearly looking for that
  // game type. Falls back to whatever format the store already remembers
  // when no filter is active. Mirrors the same store channel HostSetup
  // already reads from on mount, so no new props or prop threading.
  const handleHost = useCallback(() => {
    if (formatFilter) {
      setFormatConfig(FORMAT_DEFAULTS[formatFilter]);
    }
    onHostGame();
  }, [formatFilter, setFormatConfig, onHostGame]);

  useEffect(() => {
    // P2P mode uses a direct PeerJS code and has no lobby to subscribe to.
    if (isP2P) return;

    let cancelled = false;
    let ambientDetach: (() => void) | null = null;
    let lobbyDetach: (() => void) | null = null;

    // Delegate lobby traffic to the shared subscription socket owned by
    // `multiplayerStore`. The store re-handshakes on drops, re-sends
    // `SubscribeLobby` on reconnect, and fans out `LobbyUpdate` snapshots
    // to every subscriber — removing the duplicate handshake this
    // component previously maintained.
    (async () => {
      const detach = await subscribeLobby((next) => {
        if (cancelled) return;
        gamesRef.current = next;
        setGames(next);
      });
      if (cancelled) {
        detach?.();
        return;
      }
      if (detach === null) {
        onServerOffline?.();
        return;
      }
      lobbyDetach = detach;

      // The store's `subscribeLobby` exposes only `LobbyUpdate`-family
      // frames; `PlayerCount` and reactive `PasswordRequired` frames are
      // ambient on the same socket. Attach a thin listener to catch them
      // without opening a second WS — `ensureSubscriptionSocket` is
      // idempotent here since `subscribeLobby` has already opened it.
      const socket = await ensureSubscriptionSocket();
      if (cancelled || !socket) {
        if (!socket) onServerOffline?.();
        return;
      }
      const ambientListener = (event: MessageEvent) => {
        let msg: { type: string; data?: unknown };
        try {
          msg = JSON.parse(event.data as string) as {
            type: string;
            data?: unknown;
          };
        } catch {
          return;
        }
        if (msg.type === "PlayerCount") {
          const data = msg.data as { count: number };
          setPlayerCount(data.count);
        } else if (msg.type === "PasswordRequired") {
          // Reactive fallback: the proactive path in `handleJoinFromList`
          // opens the modal before any server round-trip, so this only
          // fires for stale rows where the client thought the room was
          // open and the server said otherwise.
          const data = msg.data as { game_code: string };
          const game = gamesRef.current.find(
            (g) => g.game_code === data.game_code,
          );
          setPasswordModal({ gameCode: data.game_code, format: game?.format });
          setPasswordInput("");
        }
      };
      socket.ws.addEventListener("message", ambientListener);
      ambientDetach = () => {
        socket.ws.removeEventListener("message", ambientListener);
      };
    })();

    return () => {
      cancelled = true;
      ambientDetach?.();
      lobbyDetach?.();
    };
  }, [isP2P, subscribeLobby, ensureSubscriptionSocket, onServerOffline]);

  const handleJoinFromList = useCallback(
    (code: string, format?: GameFormat) => {
      const game = gamesRef.current.find((g) => g.game_code === code);
      // Proactive password prompt: if the lobby row advertises a password,
      // open the modal before any server round-trip. The reactive
      // `PasswordRequired` handler above remains as a fallback for stale
      // rows (server says yes when the client thought no).
      if (game?.has_password) {
        setPasswordModal({ gameCode: code, format, context: game });
        setPasswordInput("");
        return;
      }
      onJoinGame(code, undefined, format, game);
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
      onJoinGame(
        passwordModal.gameCode,
        passwordInput,
        passwordModal.format,
        passwordModal.context,
      );
      setPasswordModal(null);
      setPasswordInput("");
    }
  }, [passwordModal, passwordInput, onJoinGame]);

  // Only show the room-type segmented filter when the visible list is
  // actually mixed. On a single-purpose deploy (all-P2P or all-server)
  // the control is noise, and hiding it matches the "don't add UI without
  // clear value" bar. Compared via `=== true` so absent/undefined entries
  // (older server builds pre-`is_p2p`) count as server-run, not unknown.
  const hasP2PRow = useMemo(() => games.some((g) => g.is_p2p === true), [games]);
  const hasServerRow = useMemo(() => games.some((g) => g.is_p2p !== true), [games]);
  const showRoomTypeFilter = hasP2PRow && hasServerRow;

  const filteredGames = useMemo(() => {
    return games.filter((g) => {
      if (formatFilter && (g.format ?? "Standard") !== formatFilter) return false;
      if (roomTypeFilter === "p2p" && g.is_p2p !== true) return false;
      if (roomTypeFilter === "server" && g.is_p2p === true) return false;
      return true;
    });
  }, [games, formatFilter, roomTypeFilter]);

  return (
    <MenuPanel className="relative z-10 mx-auto flex w-full max-w-xl flex-col gap-6 px-4 py-5">
      <div className="flex w-full items-center justify-between gap-3">
        <div className="text-[0.68rem] uppercase tracking-[0.22em] text-slate-500">
          {isP2P ? "Direct Connection" : "Online Lobby"}
        </div>
        <div className="flex items-center gap-2">
          {isServer && (
            <button
              type="button"
              onClick={() => setServerPickerOpen(true)}
              title={serverAddress}
              className="rounded-full border border-white/10 bg-black/18 px-2.5 py-0.5 font-mono text-[10px] text-slate-300 transition-colors hover:border-white/18 hover:bg-white/6"
            >
              {serverAddress.replace(/^wss?:\/\//, "").split("/")[0]}
            </button>
          )}
          {/* In P2P mode the user has no other path back to ServerPicker —
              the server-address chip above is hidden, and ServerOfflinePrompt
              only fires when we tried to use a server. Offer an explicit
              affordance so users who picked "P2P only" aren't trapped. */}
          {isP2P && (
            <button
              type="button"
              onClick={() => setServerPickerOpen(true)}
              title="Pick a server to use online lobby mode"
              className="rounded-full border border-white/10 bg-black/18 px-2.5 py-0.5 text-[10px] text-slate-300 transition-colors hover:border-white/18 hover:bg-white/6"
            >
              Pick server
            </button>
          )}
          {isServer && playerCount > 0 && (
            <span className="rounded-full bg-emerald-500/20 px-2.5 py-0.5 text-xs font-medium text-emerald-300">
              {playerCount} online
            </span>
          )}
        </div>
      </div>

      {/* Format filter -- grouped native <select>. Native is the
          mobile/tablet UX win: a 14-chip segmented control overflows
          horizontally on phone/tablet, while native <select> opens an
          OS-level full-screen picker that's already touch-optimized.
          Trigger is sized to the 44px touch-target rule. */}
      {isServer && (
        <label
          htmlFor="lobby-format-filter"
          className="flex min-h-[44px] items-center gap-2 self-start rounded-[16px] bg-black/18 px-3 py-1 ring-1 ring-white/10"
        >
          <span className="text-[0.62rem] font-medium uppercase tracking-[0.18em] text-gray-500">
            Format
          </span>
          <select
            id="lobby-format-filter"
            value={formatFilter ?? FILTER_ALL_SENTINEL}
            onChange={(e) =>
              setFormatFilter(
                e.target.value === FILTER_ALL_SENTINEL ? null : (e.target.value as GameFormat),
              )
            }
            className="bg-transparent py-1.5 text-base font-medium text-white outline-none"
          >
            <option value={FILTER_ALL_SENTINEL}>All formats</option>
            {FORMAT_FILTER_GROUPS.map(({ group, items }) => (
              <optgroup key={group} label={group}>
                {items.map((m) => (
                  <option key={m.format} value={m.format}>
                    {m.label}
                  </option>
                ))}
              </optgroup>
            ))}
          </select>
        </label>
      )}

      {isServer && showRoomTypeFilter && (
        <div className="flex rounded-[16px] bg-black/18 p-0.5 ring-1 ring-white/10">
          {ROOM_TYPE_FILTERS.map((opt) => (
            <button
              key={opt.value}
              onClick={() => setRoomTypeFilter(opt.value)}
              className={`rounded px-3 py-1 text-xs font-medium transition-colors ${
                roomTypeFilter === opt.value
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
            <div className="flex flex-col items-center gap-3 rounded-[18px] border border-dashed border-white/10 px-4 py-6 text-center">
              <p className="text-sm text-gray-400">
                {formatFilter
                  ? `No ${formatFilter} games right now.`
                  : "No open games right now."}
              </p>
              {formatFilter && (
                <button
                  type="button"
                  onClick={() => setFormatFilter(null)}
                  className={menuButtonClass({ tone: "neutral", size: "sm" })}
                >
                  Show all formats
                </button>
              )}
            </div>
          ) : (
            <div className="flex max-h-64 flex-col gap-2 overflow-y-auto">
              {filteredGames.map((game) => (
                <GameListItem
                  key={game.game_code}
                  game={game}
                  onJoin={handleJoinFromList}
                  compatible={isLobbyEntryCompatible(game.host_build_commit)}
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
            onClick={handleHost}
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

      {serverPickerOpen && (
        <ServerPicker
          onClose={() => setServerPickerOpen(false)}
          onApply={(url) => {
            useMultiplayerStore.getState().setServerAddress(url);
          }}
        />
      )}

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
