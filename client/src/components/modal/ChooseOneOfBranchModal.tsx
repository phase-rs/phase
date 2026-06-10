import { useCallback, useMemo } from "react";
import { useTranslation } from "react-i18next";

import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { DialogShell } from "./DialogShell.tsx";

function branchLabel(
  index: number,
  descriptions: string[] | undefined,
  fallback: string,
): string {
  const raw = descriptions?.[index]?.trim();
  if (raw) {
    // Display formatting only: parser-derived descriptions can be lower-case
    // oracle fragments ("create a Food token"); engine fallbacks arrive
    // already capitalized, for which this is a no-op.
    return raw.charAt(0).toUpperCase() + raw.slice(1);
  }
  return fallback;
}

export function ChooseOneOfBranchModal() {
  const { t } = useTranslation("game");
  const canActForWaitingState = useCanActForWaitingState();
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameStore((s) => s.dispatch);

  const branchCount = useMemo(() => {
    if (waitingFor?.type !== "ChooseOneOfBranch") return 0;
    return waitingFor.data.branches.length;
  }, [waitingFor]);

  const choose = useCallback(
    (index: number) => {
      dispatch({ type: "ChooseBranch", data: { index } });
    },
    [dispatch],
  );

  if (waitingFor?.type !== "ChooseOneOfBranch" || !canActForWaitingState) return null;

  const descriptions = waitingFor.data.branch_descriptions;

  return (
    <DialogShell
      eyebrow={t("chooseOneOfBranch.eyebrow")}
      title={t("chooseOneOfBranch.title")}
      subtitle={
        branchCount === 2
          ? t("chooseOneOfBranch.subtitleBinary")
          : t("chooseOneOfBranch.subtitle")
      }
      size="md"
      scrollable
    >
      <div className="px-3 py-3 lg:px-5 lg:py-5">
        <div className="flex flex-col gap-2">
          {waitingFor.data.branches.map((_, index) => (
            <button
              key={index}
              type="button"
              onClick={() => choose(index)}
              className="rounded-[16px] border border-white/8 bg-white/5 px-4 py-3 text-left transition hover:bg-white/8 hover:ring-1 hover:ring-cyan-400/30 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-cyan-400/50"
            >
              <span className="font-semibold text-white">
                {branchLabel(
                  index,
                  descriptions,
                  t("chooseOneOfBranch.optionFallback", { number: index + 1 }),
                )}
              </span>
            </button>
          ))}
        </div>
      </div>
    </DialogShell>
  );
}
