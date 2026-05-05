import { useCallback } from "react";

import { useCanActForWaitingState } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { DialogShell } from "./DialogShell.tsx";

export function ChooseOneOfBranchModal() {
  const canActForWaitingState = useCanActForWaitingState();
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameStore((s) => s.dispatch);

  const choose = useCallback(
    (index: number) => {
      dispatch({ type: "ChooseBranch", data: { index } });
    },
    [dispatch],
  );

  if (waitingFor?.type !== "ChooseOneOfBranch" || !canActForWaitingState) return null;

  return (
    <DialogShell
      eyebrow="Choice"
      title="Choose one"
      subtitle="Select the option to resolve."
      size="md"
      scrollable
    >
      <div className="px-3 py-3 lg:px-5 lg:py-5">
        <div className="flex flex-col gap-2">
          {waitingFor.data.branches.map((_, index) => (
            <button
              key={index}
              onClick={() => choose(index)}
              className="rounded-[16px] border border-white/8 bg-white/5 px-4 py-3 text-left transition hover:bg-white/8 hover:ring-1 hover:ring-cyan-400/30"
            >
              <span className="font-semibold text-white">
                {waitingFor.data.branch_descriptions?.[index] ?? `Option ${index + 1}`}
              </span>
            </button>
          ))}
        </div>
      </div>
    </DialogShell>
  );
}
