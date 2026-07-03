import { useCallback } from "react";
import { useTranslation } from "react-i18next";

import type { ReplacementCandidateSummary } from "../../adapter/types.ts";
import { useInspectHoverProps } from "../../hooks/useInspectHoverProps.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { RichLabel } from "../mana/RichLabel.tsx";
import { DialogShell } from "./DialogShell.tsx";

const EMPTY_CANDIDATES: ReplacementCandidateSummary[] = [];

/**
 * CR 616.1 / CR 614: Surfaced when the local player must apply an optional
 * replacement effect ("you may") or choose the order in which multiple
 * applicable replacements apply. The engine owns all logic and provides one
 * {@link ReplacementCandidateSummary} per option, so each button can show the
 * source object (or rule-based virtual replacement) creating that effect.
 * This component only dispatches the chosen index — no re-derivation from
 * `state.objects`.
 */
export function ReplacementModal() {
  const { t } = useTranslation("game");
  const waitingFor = useGameStore((s) => s.waitingFor);
  const dispatch = useGameStore((s) => s.dispatch);
  const hoverProps = useInspectHoverProps();

  const isReplacementChoice = waitingFor?.type === "ReplacementChoice";
  const candidateCount = isReplacementChoice
    ? waitingFor.data.candidate_count
    : 0;
  const candidates: ReplacementCandidateSummary[] = isReplacementChoice
    ? (waitingFor.data.candidates ?? EMPTY_CANDIDATES)
    : EMPTY_CANDIDATES;

  const handleChoose = useCallback(
    (index: number) => {
      dispatch({ type: "ChooseReplacement", data: { index } });
    },
    [dispatch],
  );

  if (!isReplacementChoice || candidateCount === 0) return null;

  const indices = Array.from({ length: candidateCount }, (_, i) => i);

  return (
    <DialogShell
      eyebrow={t("replacement.eyebrow")}
      title={t("replacement.title")}
      subtitle={t("replacement.subtitle")}
      size="md"
      scrollable
    >
      <div className="px-3 py-3 lg:px-5 lg:py-5">
        <div className="flex flex-col gap-2">
          {indices.map((index) => {
            const candidate = candidates[index];
            const label =
              candidate?.description ||
              t("replacement.candidateFallback", { number: index + 1 });
            return (
              <button
                key={index}
                onClick={() => handleChoose(index)}
                {...(candidate ? hoverProps(candidate.source_id) : {})}
                className="min-h-11 rounded-[16px] border border-white/8 bg-white/5 px-4 py-3 text-left transition hover:bg-white/8 hover:ring-1 hover:ring-cyan-400/40"
              >
                <span className="block font-semibold text-white">
                  <RichLabel text={label} size="sm" />
                </span>
                {candidate?.source_name && (
                  <span className="mt-0.5 block text-xs text-white/60">
                    {candidate.source_name}
                  </span>
                )}
              </button>
            );
          })}
        </div>
      </div>
    </DialogShell>
  );
}
