import { useGameDispatch } from "../../hooks/useGameDispatch";
import { useGameStore } from "../../stores/gameStore";

/**
 * Host-only panel for granting / revoking debug permission in a sandbox
 * game. Rendered inside `DebugActions` only when the local seat is the host
 * (PlayerId(0)) and `format_config.allow_debug_actions` is true. The host
 * cannot revoke their own permission — server-core enforces this; the UI
 * mirrors that by omitting the toggle for the host's row.
 */
export function GrantDebugPermissionPanel() {
  const dispatch = useGameDispatch();
  const players = useGameStore((s) => s.gameState?.players);
  const debugPermitted = useGameStore((s) => s.gameState?.debug_permitted ?? []);
  const seatOrder = useGameStore((s) => s.gameState?.seat_order);

  if (!players) return null;

  const orderedPlayers = seatOrder
    ? seatOrder
        .map((pid) => players.find((p) => p.id === pid))
        .filter((p): p is NonNullable<typeof p> => p !== undefined)
    : players;

  const opponents = orderedPlayers.filter((p) => p.id !== 0);
  if (opponents.length === 0) return null;

  return (
    <div className="mb-2 rounded border border-amber-500/30 bg-amber-500/5 px-2 py-2">
      <h4 className="mb-1 font-mono text-[10px] font-bold uppercase tracking-wider text-amber-300">
        Sandbox: Debug Permissions
      </h4>
      <p className="mb-1.5 text-[10px] leading-3 text-amber-200/70">
        Grant other players the ability to submit debug actions. Their actions
        are logged publicly.
      </p>
      <div className="flex flex-col gap-1">
        {opponents.map((p) => {
          const granted = debugPermitted.includes(p.id);
          return (
            <button
              key={p.id}
              onClick={() =>
                dispatch({
                  type: granted ? "RevokeDebugPermission" : "GrantDebugPermission",
                  data: { player_id: p.id },
                })
              }
              className={
                "flex items-center justify-between rounded px-2 py-1 text-xs transition-colors " +
                (granted
                  ? "bg-amber-500/20 text-amber-200 hover:bg-amber-500/30"
                  : "bg-gray-800 text-gray-400 hover:bg-gray-700")
              }
            >
              <span>Player {p.id + 1}</span>
              <span className="font-mono text-[10px]">
                {granted ? "REVOKE" : "GRANT"}
              </span>
            </button>
          );
        })}
      </div>
    </div>
  );
}
