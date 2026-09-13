import { useTranslation } from "react-i18next";

import { useMultiplayerStore } from "../../stores/multiplayerStore";

/** Host-measured RTT, shared with every participant in a P2P game. */
export function PlayerLatency({ playerId }: { playerId: number }) {
  const { t } = useTranslation("game");
  const latency = useMultiplayerStore((s) => s.playerLatencies[playerId]);
  const disconnected = useMultiplayerStore((s) => s.disconnectedPlayers.has(playerId));
  if (latency === undefined || disconnected) return null;
  return (
    <span
      className={`shrink-0 whitespace-nowrap text-[10px] tabular-nums ${latency === null ? "text-amber-400" : "text-gray-400"}`}
      title={t("playerLatency.description")}
    >
      {latency === null ? t("playerLatency.waiting") : playerId === 0
        ? t("playerLatency.host") : t("playerLatency.value", { ms: latency })}
    </span>
  );
}
