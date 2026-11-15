import { useTranslation } from "react-i18next";

import type { LlmDrafterState } from "../../services/llm/draftLlm";
import { useDraftStore } from "../../stores/draftStore";
import { BotIndicator } from "./BotIndicator";

const STATE_DOT: Record<LlmDrafterState, string> = {
  picking: "animate-pulse bg-white/40",
  ready: "bg-green-400",
  fallback: "bg-amber-400",
};

const STATE_BORDER: Record<LlmDrafterState, string> = {
  picking: "border-white/15",
  ready: "border-green-400/30",
  fallback: "border-amber-400/30",
};

/**
 * Which LLM drafters have picked from the pack in front of the pod, and — once
 * the player has picked — that the pack is waiting on the rest of the table.
 *
 * Renders store state only: the per-seat states come from the round the store
 * started when the pack opened, and the seat names are the engine's.
 */
export function LlmDrafterStatus({ className = "" }: { className?: string } = {}) {
  const { t } = useTranslation("draft");
  const drafters = useDraftStore((s) => s.llmDrafters);
  const awaiting = useDraftStore((s) => s.awaitingDrafters);
  const seats = useDraftStore((s) => s.view?.seats);

  if (drafters.length === 0 && !awaiting) return null;

  return (
    <div
      data-llm-drafters
      role="status"
      aria-live="polite"
      className={`flex flex-wrap items-center gap-1.5 rounded-[12px] border border-white/10 bg-black/18 px-3 py-1.5 backdrop-blur-md ${className}`}
    >
      {awaiting ? (
        <span data-llm-drafters-waiting className="text-xs font-semibold text-white">
          {t("llmDrafters.waiting")}
        </span>
      ) : (
        <span className="text-[0.68rem] uppercase tracking-[0.18em] text-white/40">
          {t("llmDrafters.label")}
        </span>
      )}
      {drafters.map(({ seat, state }) => {
        const seatName = seats?.find((entry) => entry.seat_index === seat)?.display_name
          || t("seat.label", { number: seat + 1 });
        const label = t("llmDrafters.seatStatus", {
          seat: seatName,
          status: t(`llmDrafters.${state}`),
        });
        return (
          <span
            key={seat}
            data-llm-drafter-seat={seat}
            data-llm-drafter-state={state}
            aria-label={label}
            title={label}
            className={`flex items-center gap-1 rounded-[8px] border bg-black/18 px-1.5 py-0.5 text-[11px] text-white/80 ${STATE_BORDER[state]}`}
          >
            <BotIndicator label={seatName} size="sm" />
            <span className="max-w-[10ch] truncate">{seatName}</span>
            <span className={`h-1.5 w-1.5 rounded-full ${STATE_DOT[state]}`} />
          </span>
        );
      })}
    </div>
  );
}
