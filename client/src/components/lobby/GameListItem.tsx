import type { GameFormat } from "../../adapter/types";

interface LobbyGame {
  game_code: string;
  host_name: string;
  created_at: number;
  has_password: boolean;
  format?: GameFormat;
  current_players?: number;
  max_players?: number;
  /** Display-only version string (e.g. "0.1.11"). */
  host_version?: string;
  /**
   * Git short-hash of the host's build. Used as a hard compatibility gate:
   * when the lobby list renders, rows whose commit doesn't match the
   * client's own build are disabled because the host and guest would run
   * diverged engine rules otherwise.
   */
  host_build_commit?: string;
  /** Optional host-provided label for this room, distinct from their player
   * name. When present, the lobby row shows it as the primary title with
   * the host's player name as secondary metadata. */
  room_name?: string | null;
}

interface GameListItemProps {
  game: LobbyGame;
  onJoin: (code: string, format?: GameFormat) => void;
  /**
   * When false, the row is visible but disabled with a tooltip explaining
   * the mismatch. Computed by the parent from the server's `build_commit`
   * vs the client's `__BUILD_HASH__`.
   */
  compatible?: boolean;
}

const FORMAT_BADGE_CLASSES: Record<GameFormat, string> = {
  Standard: "bg-blue-500/20 text-blue-300",
  Commander: "bg-indigo-500/20 text-indigo-300",
  Pioneer: "bg-cyan-500/20 text-cyan-300",
  Historic: "bg-sky-500/20 text-sky-300",
  Pauper: "bg-slate-500/20 text-slate-300",
  Brawl: "bg-purple-500/20 text-purple-300",
  HistoricBrawl: "bg-violet-500/20 text-violet-300",
  FreeForAll: "bg-amber-500/20 text-amber-300",
  TwoHeadedGiant: "bg-emerald-500/20 text-emerald-300",
};

const FORMAT_LABELS: Record<GameFormat, string> = {
  Standard: "STD",
  Commander: "CMD",
  Pioneer: "PIO",
  Historic: "HIS",
  Pauper: "PAU",
  Brawl: "BRL",
  HistoricBrawl: "HBR",
  FreeForAll: "FFA",
  TwoHeadedGiant: "2HG",
};

function formatWaitTime(createdAt: number): string {
  const now = Math.floor(Date.now() / 1000);
  const diff = now - createdAt;
  if (diff < 60) return "just now";
  const mins = Math.floor(diff / 60);
  if (mins < 60) return `${mins}m ago`;
  const hours = Math.floor(mins / 60);
  return `${hours}h ago`;
}

export function GameListItem({ game, onJoin, compatible = true }: GameListItemProps) {
  const format = game.format ?? "Standard";
  const badgeClass = FORMAT_BADGE_CLASSES[format];
  const formatLabel = FORMAT_LABELS[format];

  // A game is "full" when every configured seat is occupied (humans + AI).
  // The server unregisters full games on the last join, so in the happy path
  // browsers rarely see this state — but race conditions between a join and
  // the `LobbyGameRemoved` broadcast can briefly expose it, and a disabled
  // row is a clearer UX than a row that errors on click.
  const isFull =
    game.max_players != null &&
    game.current_players != null &&
    game.current_players >= game.max_players;
  const disabled = !compatible || isFull;

  const disabledTitle = !compatible
    ? `Host is on ${game.host_version || "?"} (${game.host_build_commit || "?"}) — your build is different. Refresh to update.`
    : isFull
      ? "This game is full."
      : undefined;

  return (
    <button
      onClick={() => !disabled && onJoin(game.game_code, game.format)}
      disabled={disabled}
      title={disabledTitle}
      className={
        "flex w-full items-center gap-3 rounded-[18px] border px-4 py-3 text-left transition-colors " +
        (disabled
          ? "cursor-not-allowed border-white/5 bg-black/10 opacity-60"
          : "border-white/10 bg-black/18 hover:border-white/18 hover:bg-white/6")
      }
    >
      {/* Format badge */}
      <span className={`flex-shrink-0 rounded px-1.5 py-0.5 text-xs font-semibold ${badgeClass}`}>
        {formatLabel}
      </span>

      {/* Room title and metadata. When the host set an explicit room name
          we show it as the primary title and demote the host's player name
          to the secondary line; otherwise fall back to showing the player
          name as the title (the pre-room_name behavior). */}
      <div className="min-w-0 flex-1">
        <p className="truncate text-sm font-medium text-gray-200">
          {game.room_name || game.host_name || "Anonymous"}
        </p>
        <p className="text-xs text-gray-500">
          {game.room_name && game.host_name && (
            <span className="mr-2 text-gray-400">by {game.host_name}</span>
          )}
          {formatWaitTime(game.created_at)}
          {game.host_version && (
            <span className="ml-2 font-mono text-[10px] text-gray-600">
              v{game.host_version}
              {game.host_build_commit ? `·${game.host_build_commit}` : ""}
            </span>
          )}
        </p>
      </div>

      {/* Player count */}
      {game.max_players != null && (
        <span className="flex-shrink-0 text-xs text-gray-400">
          {game.current_players ?? 1}/{game.max_players}
        </span>
      )}

      {/* Lock icon for password-protected games */}
      {game.has_password && (
        <svg
          xmlns="http://www.w3.org/2000/svg"
          viewBox="0 0 20 20"
          fill="currentColor"
          className="h-4 w-4 flex-shrink-0 text-amber-400"
          aria-label="Password protected"
        >
          <path
            fillRule="evenodd"
            d="M10 1a4.5 4.5 0 0 0-4.5 4.5V9H5a2 2 0 0 0-2 2v6a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2v-6a2 2 0 0 0-2-2h-.5V5.5A4.5 4.5 0 0 0 10 1Zm3 8V5.5a3 3 0 1 0-6 0V9h6Z"
            clipRule="evenodd"
          />
        </svg>
      )}

      {/* Game code badge */}
      <span className="flex-shrink-0 rounded-full border border-white/10 bg-black/18 px-2 py-0.5 font-mono text-xs tracking-wider text-emerald-400">
        {game.game_code}
      </span>
    </button>
  );
}

export type { LobbyGame };
