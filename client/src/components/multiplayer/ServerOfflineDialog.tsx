import { AnimatePresence, motion } from "framer-motion";
import { useTranslation } from "react-i18next";

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
  const { t } = useTranslation("multiplayer");
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
            <h2 className="mb-2 text-xl font-bold text-white">{t("serverOfflineDialog.title")}</h2>
            <p className="mb-3 text-sm text-gray-300">
              {t("serverOfflineDialog.couldNotConnect")}
            </p>
            <p className="mb-3 text-sm text-cyan-200">
              {t("serverOfflineDialog.switchedToP2P")}
            </p>
            <p className="mb-5 text-xs text-gray-400">
              {t("serverOfflineDialog.serverAddress")}
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
                {t("serverOfflineDialog.dismiss")}
              </button>
              <button
                onClick={onOpenSettings}
                className={menuButtonClass({ tone: "cyan", size: "sm" })}
              >
                {t("serverOfflineDialog.openSettings")}
              </button>
            </div>
            </MenuPanel>
          </motion.div>
        </div>
      )}
    </AnimatePresence>
  );
}
