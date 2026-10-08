import { motion, AnimatePresence } from "framer-motion";
import { useId, useRef, type RefObject } from "react";
import { useTranslation } from "react-i18next";

import { FocusScope } from "../ui/FocusScope";

interface GameConcessionAction {
  readonly kind: "game";
  readonly consequence: "ordinary-game" | "best-of-three-game";
  readonly onConfirm: () => void;
}

interface MatchConcessionAction {
  readonly kind: "match";
  readonly onConfirm: () => void;
}

interface ConcedeDialogProps {
  isOpen: boolean;
  gameAction: GameConcessionAction;
  /** Present only when the active transport supports authenticated match concession. */
  matchAction?: MatchConcessionAction;
  onCancel: () => void;
  /** Stable destination when the invoking surface supplies one explicitly. */
  returnFocusRef?: RefObject<HTMLElement | SVGElement | null>;
}

export function ConcedeDialog({
  isOpen,
  gameAction,
  matchAction,
  onCancel,
  returnFocusRef,
}: ConcedeDialogProps) {
  const { t } = useTranslation("multiplayer");
  const titleId = useId();
  const gameMessageId = useId();
  const matchMessageId = useId();
  const overlayRef = useRef<HTMLDivElement>(null);
  const dialogRef = useRef<HTMLDivElement>(null);
  const cancelRef = useRef<HTMLButtonElement>(null);
  const title = t("concedeDialog.title");
  return (
    <FocusScope
      active={isOpen}
      containerRef={dialogRef}
      ownerRef={overlayRef}
      initialFocusRef={cancelRef}
      returnFocusRef={returnFocusRef}
      onEscape={onCancel}
    >
      {({ onKeyDown }) => (
        <AnimatePresence>
          {isOpen && (
            <div
              ref={overlayRef}
              className="fixed inset-0 z-50 flex items-center justify-center"
              onKeyDown={onKeyDown}
            >
              <motion.button
                type="button"
                className="absolute inset-0 bg-black/70"
                initial={{ opacity: 0 }}
                animate={{ opacity: 1 }}
                exit={{ opacity: 0 }}
                onClick={onCancel}
                aria-label={t("common:actions.closeNamed", { name: title })}
              />
              <motion.div
                ref={dialogRef}
                role="alertdialog"
                aria-modal="true"
                aria-labelledby={titleId}
                aria-describedby={
                  matchAction
                    ? `${gameMessageId} ${matchMessageId}`
                    : gameMessageId
                }
                tabIndex={-1}
                className="relative z-10 w-80 rounded-xl bg-gray-900 p-6 text-center shadow-2xl ring-1 ring-gray-700"
                initial={{ opacity: 0, scale: 0.9 }}
                animate={{ opacity: 1, scale: 1 }}
                exit={{ opacity: 0, scale: 0.9 }}
                transition={{ type: "spring", stiffness: 300, damping: 25 }}
              >
                <h2 id={titleId} className="mb-2 text-xl font-bold text-white">
                  {title}
                </h2>
                <p id={gameMessageId} className="mb-4 text-sm text-gray-400">
                  {t(
                    gameAction.consequence === "best-of-three-game"
                      ? "concedeDialog.game.matchMessage"
                      : "concedeDialog.game.ordinaryMessage",
                  )}
                </p>
                <div className="flex justify-center gap-3">
                  <button
                    ref={cancelRef}
                    type="button"
                    onClick={onCancel}
                    className="rounded-lg bg-gray-700 px-5 py-2 text-sm font-semibold text-gray-200 transition hover:bg-gray-600"
                  >
                    {t("common:actions.cancel")}
                  </button>
                  <button
                    type="button"
                    onClick={gameAction.onConfirm}
                    className="rounded-lg bg-red-600 px-5 py-2 text-sm font-semibold text-white transition hover:bg-red-500"
                  >
                    {t("concedeDialog.game.label")}
                  </button>
                  {matchAction && (
                    <div className="flex flex-col items-center gap-1">
                      <p
                        id={matchMessageId}
                        className="max-w-36 text-center text-xs text-gray-400"
                      >
                        {t("concedeDialog.match.message")}
                      </p>
                      <button
                        type="button"
                        onClick={matchAction.onConfirm}
                        className="rounded-lg bg-red-800 px-5 py-2 text-sm font-semibold text-white transition hover:bg-red-700"
                      >
                        {t("concedeDialog.match.label")}
                      </button>
                    </div>
                  )}
                </div>
              </motion.div>
            </div>
          )}
        </AnimatePresence>
      )}
    </FocusScope>
  );
}
