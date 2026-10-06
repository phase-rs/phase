import { useTranslation } from "react-i18next";

import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { DialogShell } from "./DialogShell.tsx";

/**
 * CR 702.60a: Ripple's first decision — "you **may** reveal the top N cards of
 * your library." Declining leaves the library untouched; accepting publishes
 * the pile (CR 701.20b: the cards stay in the library) and opens the same-named
 * free-cast offers.
 */
export function RippleRevealChoiceModal() {
  const canActForWaitingState = useCanActForWaitingState();
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameStore((s) => s.dispatch);
  const { t } = useTranslation("game");

  if (waitingFor?.type !== "RippleRevealChoice") return null;
  if (!canActForWaitingState) return null;

  const count = waitingFor.data.count;

  return (
    <DialogShell
      eyebrow={t("rippleReveal.eyebrow")}
      title={t("rippleReveal.title")}
      subtitle={t("rippleReveal.subtitle", { count })}
      previewObjectId={waitingFor.data.source_id}
    >
      <div className="flex flex-col gap-2 px-3 py-3 lg:px-5 lg:py-5">
        <button
          onClick={() =>
            dispatch({
              type: "RippleChoice",
              data: { choice: { type: "Cast" } },
            })
          }
          className="rounded-[16px] border border-white/8 bg-white/5 px-4 py-3 text-left transition hover:bg-white/8 hover:ring-1 hover:ring-cyan-400/30"
        >
          <span className="font-semibold text-white">
            {t("rippleReveal.reveal", { count })}
          </span>
        </button>
        <button
          onClick={() =>
            dispatch({
              type: "RippleChoice",
              data: { choice: { type: "Decline" } },
            })
          }
          className="rounded-[16px] border border-white/8 bg-white/5 px-4 py-3 text-left transition hover:bg-white/8 hover:ring-1 hover:ring-amber-400/30"
        >
          <span className="font-semibold text-white">
            {t("rippleReveal.decline")}
          </span>
        </button>
      </div>
    </DialogShell>
  );
}
