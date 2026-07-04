import { motion } from "framer-motion";
import { useTranslation } from "react-i18next";

import { useMultiplayerStore } from "../../stores/multiplayerStore";

const STATUS_COLORS = {
  connected: "#22c55e",    // green-500
  connecting: "#eab308",   // yellow-500
  disconnected: "#ef4444", // red-500
} as const;

export function ConnectionDot() {
  const { t } = useTranslation("multiplayer");
  const connectionStatus = useMultiplayerStore((s) => s.connectionStatus);
  const latencyMs = useMultiplayerStore((s) => s.latencyMs);
  const color = STATUS_COLORS[connectionStatus];
  const label = t(`connectionDot.${connectionStatus}`);

  const latencyLabel =
    connectionStatus === "connected" && latencyMs != null
      ? `${latencyMs}ms`
      : null;

  const latencyColor =
    latencyMs != null
      ? latencyMs < 100
        ? "text-green-400"
        : latencyMs < 250
          ? "text-yellow-400"
          : "text-red-400"
      : "";

  return (
    <div
      className="flex h-7 items-center gap-1.5 rounded-md bg-white/6 px-1.5"
      title={label}
    >
      {connectionStatus === "connecting" ? (
        <motion.div
          className="h-2 w-2 rounded-full"
          style={{ backgroundColor: color }}
          animate={{ opacity: [1, 0.3, 1] }}
          transition={{ duration: 1.5, repeat: Infinity, ease: "easeInOut" }}
        />
      ) : (
        <div
          className="h-2 w-2 rounded-full"
          style={{ backgroundColor: color }}
        />
      )}
      <span className="hidden text-[10px] font-medium text-gray-500 2xl:inline">{label}</span>
      {latencyLabel && (
        <span className={`hidden text-[10px] font-medium 2xl:inline ${latencyColor}`}>
          {latencyLabel}
        </span>
      )}
    </div>
  );
}
