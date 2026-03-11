import { motion } from "framer-motion";

import { useMultiplayerStore } from "../../stores/multiplayerStore";

const STATUS_COLORS = {
  connected: "#22c55e",    // green-500
  connecting: "#eab308",   // yellow-500
  disconnected: "#ef4444", // red-500
} as const;

const STATUS_LABELS = {
  connected: "Connected",
  connecting: "Connecting...",
  disconnected: "Disconnected",
} as const;

export function ConnectionDot() {
  const connectionStatus = useMultiplayerStore((s) => s.connectionStatus);
  const color = STATUS_COLORS[connectionStatus];
  const label = STATUS_LABELS[connectionStatus];

  return (
    <div
      className="fixed right-14 top-3 z-40 flex items-center gap-1.5"
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
      <span className="text-[10px] font-medium text-gray-500">{label}</span>
    </div>
  );
}
