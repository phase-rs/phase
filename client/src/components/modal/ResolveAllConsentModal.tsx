import { useCallback } from "react";
import { useTranslation } from "react-i18next";

import { dispatchResolveAll } from "../../game/dispatch.ts";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { DialogShell } from "./DialogShell.tsx";

/**
 * Engine-authored Resolve All consent prompt. The browser only presents the
 * representative and echoes a finite Grant/Decline action; authorization and
 * the subsequent safe prefix stay entirely in the engine.
 */
export function ResolveAllConsentModal({ playerId }: { playerId: number }) {
  const { t } = useTranslation("game");
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameDispatch();

  const visible =
    waitingFor?.type === "ResolveAllConsent" &&
    waitingFor.data.representative === playerId;

  const respond = useCallback(
    async (decision: "Grant" | "Decline") => {
      if (waitingFor?.type !== "ResolveAllConsent") return;
      const grantedEpoch = waitingFor.data.epoch;
      await dispatch({
        type: "RespondResolveAllConsent",
        data: { epoch: grantedEpoch, decision: { type: decision } },
      });
      const waitingForAfterSubmission = useGameStore.getState().gameState?.waiting_for;
      if (
        decision === "Grant" &&
        waitingForAfterSubmission?.type === "ResolveAllReady" &&
        waitingForAfterSubmission.data.epoch === grantedEpoch
      ) {
        await dispatchResolveAll(playerId, []);
      }
    },
    [dispatch, playerId, waitingFor],
  );

  if (!visible) return null;

  return (
    <DialogShell
      eyebrow={t("resolveAllConsent.eyebrow")}
      title={t("resolveAllConsent.title")}
      subtitle={t("resolveAllConsent.subtitle")}
      size="sm"
    >
      <div className="flex gap-3 px-5 py-5">
        <button
          className="flex-1 rounded-xl bg-emerald-500 px-4 py-3 font-semibold text-emerald-950 transition hover:bg-emerald-400"
          onClick={() => void respond("Grant")}
        >
          {t("resolveAllConsent.grant")}
        </button>
        <button
          className="flex-1 rounded-xl bg-gray-700 px-4 py-3 font-semibold text-white transition hover:bg-gray-600"
          onClick={() => void respond("Decline")}
        >
          {t("resolveAllConsent.decline")}
        </button>
      </div>
    </DialogShell>
  );
}
