import { useCallback } from "react";
import { useTranslation } from "react-i18next";

import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { DialogShell } from "./DialogShell.tsx";

/**
 * UI for the engine-proved finite pre-cast copy route. Every value is an
 * engine-issued route count, epoch, or breakpoint id; the client performs no
 * life, copy-count, or route validation.
 */
export function PrecastCopyShortcutOfferModal() {
  const { t } = useTranslation("game");
  const canAct = useCanActForWaitingState();
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameStore((s) => s.dispatch);

  const handlePropose = useCallback(() => {
    if (waitingFor?.type !== "PrecastCopyShortcutOffer") return;
    // The sole route's opaque id is its epoch. The engine validates both
    // independently and rejects stale or fabricated submissions.
    dispatch({
      type: "PrecastCopyShortcut",
      data: {
        epoch: waitingFor.data.epoch,
        response: { type: "Propose", data: { route_id: waitingFor.data.epoch } },
      },
    });
  }, [dispatch, waitingFor]);

  const handleDecline = useCallback(() => {
    if (waitingFor?.type !== "PrecastCopyShortcutOffer") return;
    dispatch({
      type: "PrecastCopyShortcut",
      data: { epoch: waitingFor.data.epoch, response: { type: "Decline" } },
    });
  }, [dispatch, waitingFor]);

  if (waitingFor?.type !== "PrecastCopyShortcutOffer" || !canAct) return null;

  return (
    <DialogShell
      title={t("precastShortcut.offerTitle")}
      subtitle={t("precastShortcut.offerSubtitle")}
      size="md"
      footer={
        <div className="flex flex-col gap-3 sm:flex-row sm:justify-end">
          <button
            onClick={handlePropose}
            className="min-h-11 rounded-[16px] bg-cyan-500 px-6 py-2 font-semibold text-slate-950 shadow-[0_14px_34px_rgba(6,182,212,0.28)] transition hover:bg-cyan-400"
          >
            {t("precastShortcut.propose")}
          </button>
          <button
            onClick={handleDecline}
            className="min-h-11 rounded-[16px] border border-white/8 bg-white/5 px-6 py-2 font-semibold text-slate-200 transition hover:bg-white/8"
          >
            {t("comboShortcut.decline")}
          </button>
        </div>
      }
    >
      <p className="px-3 py-3 text-sm text-slate-300 lg:px-5 lg:py-5">
        {t("precastShortcut.routeCount", { count: waitingFor.data.route_count })}
      </p>
    </DialogShell>
  );
}

export function RespondToPrecastCopyShortcutModal() {
  const { t } = useTranslation("game");
  const canAct = useCanActForWaitingState();
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameStore((s) => s.dispatch);

  const handleAccept = useCallback(() => {
    if (waitingFor?.type !== "RespondToPrecastCopyShortcut") return;
    dispatch({
      type: "PrecastCopyShortcut",
      data: { epoch: waitingFor.data.epoch, response: { type: "Accept" } },
    });
  }, [dispatch, waitingFor]);

  const handleShorten = useCallback(
    (breakpointId: number) => {
      if (waitingFor?.type !== "RespondToPrecastCopyShortcut") return;
      dispatch({
        type: "PrecastCopyShortcut",
        data: {
          epoch: waitingFor.data.epoch,
          response: { type: "Shorten", data: { breakpoint_id: breakpointId } },
        },
      });
    },
    [dispatch, waitingFor],
  );

  if (waitingFor?.type !== "RespondToPrecastCopyShortcut" || !canAct) return null;

  const breakpointIds = waitingFor.data.breakpoint_ids ?? [];
  return (
    <DialogShell
      title={t("precastShortcut.respondTitle")}
      subtitle={t("precastShortcut.respondSubtitle")}
      size="md"
      footer={
        <div className="flex flex-col gap-3 sm:flex-row sm:justify-end">
          <button
            onClick={handleAccept}
            className="min-h-11 rounded-[16px] bg-cyan-500 px-6 py-2 font-semibold text-slate-950 shadow-[0_14px_34px_rgba(6,182,212,0.28)] transition hover:bg-cyan-400"
          >
            {t("comboShortcut.accept")}
          </button>
        </div>
      }
    >
      <div className="flex flex-col gap-3 px-3 py-3 lg:px-5 lg:py-5">
        {breakpointIds.map((breakpointId) => (
          <button
            key={breakpointId}
            onClick={() => handleShorten(breakpointId)}
            className="min-h-11 rounded-[16px] border border-white/8 bg-white/5 px-4 py-2 text-left font-semibold text-slate-200 transition hover:bg-white/8"
          >
            {t("precastShortcut.shortenAtBreakpoint", { breakpointId })}
          </button>
        ))}
      </div>
    </DialogShell>
  );
}
