import { useEffect, useState } from "react";
import { motion, AnimatePresence } from "framer-motion";
import { Trans, useTranslation } from "react-i18next";

interface DisconnectChoiceDialogProps {
  isOpen: boolean;
  playerLabel: string;
  /** Total grace period in milliseconds (e.g., 30000 for 30s). */
  gracePeriodMs: number;
  /**
   * "Pause and wait": cancel the grace timer and hold the game indefinitely
   * until the player reconnects or the host explicitly continues. Adapter
   * call: `holdForReconnect(playerId)`.
   */
  onPauseAndWait: () => void;
  /**
   * "Continue without them": auto-concede the disconnected player. Adapter
   * call: `concedeDisconnected(playerId)`.
   */
  onContinueWithout: () => void;
  /**
   * Called when the dialog closes for any reason (countdown expiry,
   * reconnection, or user choice). Allows the parent to clear the trigger
   * event.
   */
  onDismiss: () => void;
}

/**
 * Host-only modal shown when a guest disconnects mid-game in a 3-4p P2P
 * session. Counts down the grace window as an advisory timer; on expiry it
 * defaults to WAITING — it dismisses WITHOUT conceding, never auto-eliminating
 * a dropped player. The host can still explicitly choose "Continue without
 * them" while the dialog is up. The parent unmounts (sets `isOpen=false`)
 * when the player reconnects, dismissing the dialog naturally.
 */
export function DisconnectChoiceDialog({
  isOpen,
  playerLabel,
  gracePeriodMs,
  onPauseAndWait,
  onContinueWithout,
  onDismiss,
}: DisconnectChoiceDialogProps) {
  const { t } = useTranslation("game");
  const [secondsRemaining, setSecondsRemaining] = useState(
    Math.ceil(gracePeriodMs / 1000),
  );

  // Reset countdown when the dialog opens with a new player.
  useEffect(() => {
    if (!isOpen) return;
    setSecondsRemaining(Math.ceil(gracePeriodMs / 1000));
  }, [isOpen, gracePeriodMs, playerLabel]);

  // Tick the countdown. On expiry, default to WAITING — dismiss without
  // conceding. A dropped player is never auto-conceded; the host keeps the
  // explicit "Continue without them" button while the dialog is up.
  useEffect(() => {
    if (!isOpen) return;
    if (secondsRemaining <= 0) {
      onDismiss();
      return;
    }
    const id = setTimeout(() => setSecondsRemaining((n) => n - 1), 1000);
    return () => clearTimeout(id);
  }, [isOpen, secondsRemaining, onDismiss]);

  return (
    <AnimatePresence>
      {isOpen && (
        <div className="fixed inset-0 z-50 flex items-center justify-center">
          <motion.div
            className="absolute inset-0 bg-black/70"
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
          />
          <motion.div
            className="relative z-10 w-96 rounded-xl bg-gray-900 p-6 text-center shadow-2xl ring-1 ring-gray-700"
            initial={{ opacity: 0, scale: 0.9 }}
            animate={{ opacity: 1, scale: 1 }}
            exit={{ opacity: 0, scale: 0.9 }}
            transition={{ type: "spring", stiffness: 300, damping: 25 }}
          >
            <h2 className="mb-2 text-xl font-bold text-white">
              {t("disconnectDialog.title", { name: playerLabel })}
            </h2>
            <p className="mb-6 text-sm text-gray-400">
              <Trans
                t={t}
                i18nKey="disconnectDialog.reconnecting"
                values={{ seconds: secondsRemaining }}
                components={{ seconds: <span className="font-mono text-amber-300" /> }}
              />
            </p>
            <div className="flex justify-center gap-3">
              <button
                onClick={() => {
                  onPauseAndWait();
                  onDismiss();
                }}
                className="rounded-lg bg-gray-700 px-5 py-2 text-sm font-semibold text-gray-200 transition hover:bg-gray-600"
              >
                {t("disconnectDialog.pauseAndWait")}
              </button>
              <button
                onClick={() => {
                  onContinueWithout();
                  onDismiss();
                }}
                className="rounded-lg bg-red-600 px-5 py-2 text-sm font-semibold text-white transition hover:bg-red-500"
              >
                {t("disconnectDialog.continueWithout")}
              </button>
            </div>
          </motion.div>
        </div>
      )}
    </AnimatePresence>
  );
}
