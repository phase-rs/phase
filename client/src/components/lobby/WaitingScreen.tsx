import { motion } from "framer-motion";

import { menuButtonClass } from "../menu/buttonStyles";

interface WaitingScreenProps {
  gameCode: string;
  isPublic: boolean;
  onCancel: () => void;
}

export function WaitingScreen({ gameCode, isPublic, onCancel }: WaitingScreenProps) {
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      <div className="absolute inset-0 bg-black/80" />

      <div className="relative z-10 flex flex-col items-center gap-6 rounded-xl bg-gray-900 p-10 shadow-2xl ring-1 ring-gray-700">
        <h2 className="text-xl font-bold text-white">Waiting for Opponent</h2>

        {/* Game code */}
        <div className="text-center">
          <p className="mb-2 text-xs font-medium uppercase tracking-wider text-gray-400">
            Game Code
          </p>
          <p className="font-mono text-4xl font-bold tracking-widest text-emerald-400">
            {gameCode}
          </p>
        </div>

        {/* Local IP hint */}
        <p className="text-xs text-gray-500">
          LAN: {window.location.hostname || "localhost"}
        </p>

        {/* Public badge */}
        {isPublic && (
          <span className="rounded-full bg-emerald-500/20 px-3 py-1 text-xs font-medium text-emerald-300">
            Listed in lobby
          </span>
        )}

        {/* Helper text */}
        <p className="max-w-xs text-center text-sm text-gray-400">
          Share the code with a friend, or wait for someone from the lobby.
        </p>

        {/* Animated waiting dots */}
        <div className="flex gap-2">
          {[0, 1, 2].map((i) => (
            <motion.div
              key={i}
              className="h-2.5 w-2.5 rounded-full bg-emerald-400"
              animate={{ scale: [1, 1.5, 1], opacity: [0.4, 1, 0.4] }}
              transition={{
                duration: 1.2,
                repeat: Infinity,
                delay: i * 0.2,
                ease: "easeInOut",
              }}
            />
          ))}
        </div>

        {/* Cancel button */}
        <button
          onClick={onCancel}
          className={menuButtonClass({ tone: "neutral", size: "sm" })}
        >
          Cancel
        </button>
      </div>
    </div>
  );
}
