import { motion } from "framer-motion";
import { useTranslation } from "react-i18next";

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
  const { t } = useTranslation("multiplayer");
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
          {t("serverOfflinePrompt.title")}
        </h2>
        <p className="mt-3 text-sm leading-6 text-slate-300">
          {t("serverOfflinePrompt.message")}
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
            {t("serverOfflinePrompt.keepTrying")}
          </button>
          <button
            type="button"
            onClick={onUseDirect}
            className={menuButtonClass({ tone: "cyan", size: "sm" })}
          >
            {t("serverOfflinePrompt.useDirectCode")}
          </button>
        </div>
      </motion.div>
    </div>
  );
}
