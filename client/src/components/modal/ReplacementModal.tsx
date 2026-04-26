import { useCallback } from "react";

import { useGameStore } from "../../stores/gameStore.ts";
import { DialogShell } from "./DialogShell.tsx";

export function ReplacementModal() {
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameStore((s) => s.dispatch);

  const isReplacementChoice = waitingFor?.type === "ReplacementChoice";
  const candidateCount = isReplacementChoice
    ? waitingFor.data.candidate_count
    : 0;
  const candidateDescriptions: string[] = isReplacementChoice
    ? (waitingFor.data.candidate_descriptions ?? [])
    : [];

  const handleChoose = useCallback(
    (index: number) => {
      dispatch({ type: "ChooseReplacement", data: { index } });
    },
    [dispatch],
  );

  if (!isReplacementChoice || candidateCount === 0) return null;

  const candidates = Array.from({ length: candidateCount }, (_, i) => i);

  return (
    <DialogShell
      eyebrow="Resolution Order"
      title="Replacement Effects"
      subtitle="Choose which replacement effect applies first."
      size="md"
      scrollable
    >
      <div className="px-3 py-3 lg:px-5 lg:py-5">
        <div className="flex flex-col gap-2">
          {candidates.map((index) => {
            const desc = candidateDescriptions[index];
            return (
              <button
                key={index}
                onClick={() => handleChoose(index)}
                className="min-h-11 rounded-[16px] border border-white/8 bg-white/5 px-4 py-3 text-left transition hover:bg-white/8 hover:ring-1 hover:ring-cyan-400/40"
              >
                <span className="font-semibold text-white">
                  {desc || `Replacement Effect ${index + 1}`}
                </span>
              </button>
            );
          })}
        </div>
      </div>
    </DialogShell>
  );
}
