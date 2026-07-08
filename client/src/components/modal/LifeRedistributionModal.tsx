import { useCallback } from "react";
import { useTranslation } from "react-i18next";

import { useCanActForWaitingState, usePerspectivePlayerId } from "../../hooks/usePlayerId.ts";
import { getPlayerDisplayName } from "../../stores/multiplayerStore.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { DialogShell } from "./DialogShell.tsx";

/**
 * CR 119.7 + CR 119.8: Renders the engine-enumerated life-total redistribution options
 * (Reverse the Sands, The Doctor's Tomb) and submits the chosen index. Pure
 * display: the engine computes which assignments are legal and their ordering
 * (index 0 is always the "keep current totals" identity); this component only
 * labels the engine-provided data and dispatches the selection.
 */
export function LifeRedistributionModal() {
  const { t } = useTranslation("game");
  const canActForWaitingState = useCanActForWaitingState();
  const perspectiveId = usePerspectivePlayerId();
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameStore((s) => s.dispatch);

  const choose = useCallback(
    (optionIndex: number) => {
      dispatch({ type: "SubmitLifeRedistribution", data: { option_index: optionIndex } });
    },
    [dispatch],
  );

  if (waitingFor?.type !== "RedistributeLifeTotals" || !canActForWaitingState) return null;

  const { options } = waitingFor.data;

  return (
    <DialogShell
      eyebrow={t("redistributeLifeTotals.eyebrow")}
      title={t("redistributeLifeTotals.title")}
      subtitle={t("redistributeLifeTotals.subtitle")}
      size="md"
      scrollable
    >
      <div className="px-3 py-3 lg:px-5 lg:py-5">
        <div className="flex flex-col gap-2">
          {options.map((option, index) => (
            <button
              key={index}
              type="button"
              onClick={() => choose(index)}
              className="rounded-[16px] border border-white/8 bg-white/5 px-4 py-3 text-left transition hover:bg-white/8 hover:ring-1 hover:ring-cyan-400/30 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-cyan-400/50"
            >
              <span className="font-semibold text-white">
                {index === 0
                  ? t("redistributeLifeTotals.keepCurrent")
                  : option.assignment
                      .map(([playerId, life]) =>
                        t("redistributeLifeTotals.playerLife", {
                          player: getPlayerDisplayName(playerId, perspectiveId),
                          life,
                        }),
                      )
                      .join(", ")}
              </span>
            </button>
          ))}
        </div>
      </div>
    </DialogShell>
  );
}
