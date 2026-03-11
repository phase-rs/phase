import type { GameFormat } from "../../adapter/types";

interface LobbyGame {
  game_code: string;
  host_name: string;
  created_at: number;
  has_password: boolean;
  format?: GameFormat;
  current_players?: number;
  max_players?: number;
}

interface GameListItemProps {
  game: LobbyGame;
  onJoin: (code: string) => void;
}

const FORMAT_BADGE_CLASSES: Record<GameFormat, string> = {
  Standard: "bg-blue-500/20 text-blue-300",
  Commander: "bg-indigo-500/20 text-indigo-300",
  FreeForAll: "bg-amber-500/20 text-amber-300",
  TwoHeadedGiant: "bg-emerald-500/20 text-emerald-300",
};

const FORMAT_LABELS: Record<GameFormat, string> = {
  Standard: "STD",
  Commander: "CMD",
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

export function GameListItem({ game, onJoin }: GameListItemProps) {
  const format = game.format ?? "Standard";
  const badgeClass = FORMAT_BADGE_CLASSES[format];
  const formatLabel = FORMAT_LABELS[format];

  return (
    <button
      onClick={() => onJoin(game.game_code)}
      className="flex w-full items-center gap-3 rounded-lg border border-gray-700 bg-gray-800/60 px-4 py-3 text-left transition-colors hover:border-gray-500 hover:bg-gray-800"
    >
      {/* Format badge */}
      <span className={`flex-shrink-0 rounded px-1.5 py-0.5 text-xs font-semibold ${badgeClass}`}>
        {formatLabel}
      </span>

      {/* Host name and metadata */}
      <div className="min-w-0 flex-1">
        <p className="truncate text-sm font-medium text-gray-200">
          {game.host_name || "Anonymous"}
        </p>
        <p className="text-xs text-gray-500">{formatWaitTime(game.created_at)}</p>
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
      <span className="flex-shrink-0 rounded bg-gray-700 px-2 py-0.5 font-mono text-xs tracking-wider text-emerald-400">
        {game.game_code}
      </span>
    </button>
  );
}

export type { LobbyGame };
