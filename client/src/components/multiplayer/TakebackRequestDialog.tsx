import { motion, AnimatePresence } from "framer-motion";
import { useTranslation } from "react-i18next";

interface TakebackRequestDialogProps {
  isOpen: boolean;
  /** Display name of the player who requested the takeback. */
  requesterName: string;
  /** True when the local player is the one who made the request — shows a
   * "waiting for approval" message with a cancel option instead of the
   * approve/decline choice. */
  isOwnRequest: boolean;
  onApprove: () => void;
  onDecline: () => void;
  onCancel: () => void;
}

/**
 * GH #1507: shown to every seat while a "request takeback" is pending.
 * The requester sees a waiting state with the option to withdraw; every
 * other human seat sees an approve/decline choice. The dialog closes itself
 * when the parent observes `TakebackResolved` and sets `isOpen` to false.
 */
export function TakebackRequestDialog({
  isOpen,
  requesterName,
  isOwnRequest,
  onApprove,
  onDecline,
  onCancel,
}: TakebackRequestDialogProps) {
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
          />
          <motion.div
            className="relative z-10 w-96 rounded-xl bg-gray-900 p-6 text-center shadow-2xl ring-1 ring-gray-700"
            initial={{ opacity: 0, scale: 0.9 }}
            animate={{ opacity: 1, scale: 1 }}
            exit={{ opacity: 0, scale: 0.9 }}
            transition={{ type: "spring", stiffness: 300, damping: 25 }}
          >
            {isOwnRequest ? (
              <>
                <h2 className="mb-2 text-xl font-bold text-white">
                  {t("takebackDialog.ownTitle")}
                </h2>
                <p className="mb-6 text-sm text-gray-400">{t("takebackDialog.ownMessage")}</p>
                <div className="flex justify-center gap-3">
                  <button
                    onClick={onCancel}
                    className="rounded-lg bg-gray-700 px-5 py-2 text-sm font-semibold text-gray-200 transition hover:bg-gray-600"
                  >
                    {t("takebackDialog.cancel")}
                  </button>
                </div>
              </>
            ) : (
              <>
                <h2 className="mb-2 text-xl font-bold text-white">
                  {t("takebackDialog.requestedTitle", { name: requesterName })}
                </h2>
                <p className="mb-6 text-sm text-gray-400">
                  {t("takebackDialog.requestedMessage")}
                </p>
                <div className="flex justify-center gap-3">
                  <button
                    onClick={onDecline}
                    className="rounded-lg bg-gray-700 px-5 py-2 text-sm font-semibold text-gray-200 transition hover:bg-gray-600"
                  >
                    {t("takebackDialog.decline")}
                  </button>
                  <button
                    onClick={onApprove}
                    className="rounded-lg bg-emerald-600 px-5 py-2 text-sm font-semibold text-white transition hover:bg-emerald-500"
                  >
                    {t("takebackDialog.approve")}
                  </button>
                </div>
              </>
            )}
          </motion.div>
        </div>
      )}
    </AnimatePresence>
  );
}
