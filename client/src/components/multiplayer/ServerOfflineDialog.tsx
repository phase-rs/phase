import { AnimatePresence, motion } from "framer-motion";

import { MenuPanel } from "../menu/MenuShell";
import { menuButtonClass } from "../menu/buttonStyles";

interface ServerOfflineDialogProps {
  isOpen: boolean;
  serverAddress: string;
  onOpenSettings: () => void;
  onClose: () => void;
}

export function ServerOfflineDialog({
  isOpen,
  serverAddress,
  onOpenSettings,
  onClose,
}: ServerOfflineDialogProps) {
  return (
    <AnimatePresence>
      {isOpen && (
        <div className="fixed inset-0 z-50 flex items-center justify-center">
          <motion.div
            className="absolute inset-0 bg-black/70"
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
            onClick={onClose}
          />

          <motion.div
            className="relative z-10 w-full max-w-sm"
            initial={{ opacity: 0, scale: 0.92 }}
            animate={{ opacity: 1, scale: 1 }}
            exit={{ opacity: 0, scale: 0.92 }}
            transition={{ type: "spring", stiffness: 320, damping: 26 }}
          >
            <MenuPanel className="p-6">
            <h2 className="mb-2 text-xl font-bold text-white">Server Offline</h2>
            <p className="mb-3 text-sm text-gray-300">
              Couldn&apos;t connect to the dedicated multiplayer server.
            </p>
            <p className="mb-3 text-sm text-cyan-200">
              Switched to P2P-only mode for this session.
            </p>
            <p className="mb-5 text-xs text-gray-400">
              Server address:
              {" "}
              <code className="rounded bg-gray-800 px-1.5 py-0.5 text-cyan-300">
                {serverAddress}
              </code>
            </p>

            <div className="flex justify-end gap-2">
              <button
                onClick={onClose}
                className={menuButtonClass({ tone: "neutral", size: "sm" })}
              >
                Dismiss
              </button>
              <button
                onClick={onOpenSettings}
                className={menuButtonClass({ tone: "cyan", size: "sm" })}
              >
                Open Settings
              </button>
            </div>
            </MenuPanel>
          </motion.div>
        </div>
      )}
    </AnimatePresence>
  );
}
