import { useTranslation } from "react-i18next";

import type { MatchOutcome } from "../../services/matchHistoryPersistence";

interface RecentFormBadgeProps {
  outcomes: MatchOutcome[];
  /** How many recent games to show (default: 10). */
  count?: number;
  className?: string;
}

const PIP_CLASS: Record<MatchOutcome, string> = {
  win: "bg-emerald-500",
  loss: "bg-red-600",
  draw: "bg-slate-500",
};

const PIP_LABEL: Record<MatchOutcome, string> = {
  win: "W",
  loss: "L",
  draw: "D",
};

/** Shows the last N game results as colored pips (green=win, red=loss, grey=draw). */
export function RecentFormBadge({ outcomes, count = 10, className = "" }: RecentFormBadgeProps) {
  const { t } = useTranslation("history");
  const recent = outcomes.slice(0, count);
  if (recent.length === 0) return null;

  return (
    <div className={`flex flex-col gap-1 ${className}`}>
      <span className="text-xs text-slate-500">
        {t("recentForm.label", { count: recent.length })}
      </span>
      <div className="flex items-center gap-0.5">
        {recent.map((outcome, i) => (
          <span
            // eslint-disable-next-line react/no-array-index-key
            key={i}
            title={PIP_LABEL[outcome]}
            className={`flex h-5 w-5 items-center justify-center rounded-sm text-[9px] font-bold text-white ${PIP_CLASS[outcome]}`}
          >
            {PIP_LABEL[outcome]}
          </span>
        ))}
      </div>
    </div>
  );
}
