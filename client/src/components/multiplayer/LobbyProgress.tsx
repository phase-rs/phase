import { motion } from "framer-motion";
import { useTranslation } from "react-i18next";

interface LobbyProgressProps {
  joined: number;
  total: number;
  /** Optional room code to display so the host can share it with guests. */
  roomCode?: string;
}

/**
 * Pre-game lobby progress display for 3-4p P2P games. Replaces the
 * single-shot `waitingForGuest` spinner. Shown by `GameProvider` (host and
 * already-joined guests) until `joined === total`, at which point
 * `game_setup` arrives and the in-game UI takes over.
 */
export function LobbyProgress({ joined, total, roomCode }: LobbyProgressProps) {
  const { t } = useTranslation("multiplayer");
  const dots = Array.from({ length: total }, (_, i) => i < joined);
  return (
    <div className="fixed inset-0 z-30 flex flex-col items-center justify-center gap-6 bg-black/85 text-white">
      <motion.div
        className="text-2xl font-bold"
        initial={{ opacity: 0, y: 8 }}
        animate={{ opacity: 1, y: 0 }}
      >
        {t("lobbyProgress.waiting")}
      </motion.div>
      <div className="flex items-center gap-3">
        {dots.map((filled, i) => (
          <span
            key={i}
            className={
              "h-3 w-3 rounded-full transition-colors " +
              (filled
                ? "bg-emerald-400 ring-2 ring-emerald-300/40"
                : "bg-gray-700 ring-1 ring-gray-600")
            }
          />
        ))}
      </div>
      <div className="text-sm text-gray-300">
        {t("lobbyProgress.playersReady", { joined, total })}
      </div>
      {roomCode ? (
        <div className="mt-2 rounded-lg bg-gray-800 px-4 py-2 font-mono text-lg tracking-widest text-cyan-200">
          {roomCode}
        </div>
      ) : null}
    </div>
  );
}
