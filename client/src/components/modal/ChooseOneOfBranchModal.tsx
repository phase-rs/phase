import { useCallback, useMemo } from "react";
import { useTranslation } from "react-i18next";

import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { DialogShell } from "./DialogShell.tsx";

type BranchWire = {
  description?: string;
  effect?: { type?: string; name?: string };
};

function tokenNameFromBranch(branch: unknown): string | null {
  if (!branch || typeof branch !== "object") return null;
  const wire = branch as BranchWire;
  if (wire.effect?.type === "Token" && wire.effect.name) {
    return wire.effect.name;
  }
  return null;
}

function branchLabel(
  index: number,
  descriptions: string[] | undefined,
  branch: unknown,
  fallback: string,
): string {
  const raw = descriptions?.[index]?.trim();
  if (raw) {
    // Engine descriptions are lower-case oracle fragments ("create a Food token").
    return raw.charAt(0).toUpperCase() + raw.slice(1);
  }
  const tokenName = tokenNameFromBranch(branch);
  if (tokenName) {
    return `Create a ${tokenName} token`;
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
          {waitingFor.data.branches.map((branch, index) => (
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
                  branch,
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
