import { motion } from "framer-motion";

import { menuButtonClass } from "../menu/buttonStyles";

interface ServerOfflinePromptProps {
  /** Non-dismissive: the user must pick one of the two options. */
  onUseDirect: () => void;
  onKeepWaiting: () => void;
  /** Server URL the client couldn't reach — shown so users hosting their own
   * instance can diagnose an obvious misconfiguration. */
  serverAddress?: string;
}

export function ServerOfflinePrompt({
  onUseDirect,
  onKeepWaiting,
  serverAddress,
}: ServerOfflinePromptProps) {
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      <div className="absolute inset-0 bg-black/70" />
      <motion.div
        initial={{ opacity: 0, scale: 0.96 }}
        animate={{ opacity: 1, scale: 1 }}
        transition={{ duration: 0.18 }}
        className="relative z-10 w-full max-w-md rounded-[22px] border border-white/10 bg-[#0b1020]/96 p-6 shadow-2xl backdrop-blur-md"
      >
        <h2 className="text-base font-semibold text-white">
          Matchmaking unreachable
        </h2>
        <p className="mt-3 text-sm leading-6 text-slate-300">
          We couldn&apos;t reach the game server, so the public lobby is
          unavailable right now. You can still play by sharing a direct code
          with a friend.
        </p>
        {serverAddress && (
          <p className="mt-2 font-mono text-[10px] text-slate-500 break-all">
            {serverAddress}
          </p>
        )}
        <div className="mt-5 flex justify-end gap-2">
          <button
            type="button"
            onClick={onKeepWaiting}
            className={menuButtonClass({ tone: "neutral", size: "sm" })}
          >
            Keep trying
          </button>
          <button
            type="button"
            onClick={onUseDirect}
            className={menuButtonClass({ tone: "cyan", size: "sm" })}
          >
            Use direct code
          </button>
        </div>
      </motion.div>
    </div>
  );
}
