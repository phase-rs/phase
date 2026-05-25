import { useTranslation } from "react-i18next";

import { useMultiplayerDraftStore } from "../../stores/multiplayerDraftStore";

// ── Component ───────────────────────────────────────────────────────────

export function PickTimer() {
  const { t } = useTranslation("draft");
  const timerRemainingMs = useMultiplayerDraftStore((s) => s.timerRemainingMs);
  const podPolicy = useMultiplayerDraftStore((s) => s.view?.pod_policy);

  if (timerRemainingMs === null || podPolicy !== "Competitive") return null;

  const seconds = Math.ceil(timerRemainingMs / 1000);
  const isWarning = timerRemainingMs <= 10_000;

  return (
    <div className="flex items-center justify-center gap-2 rounded-[16px] border border-white/10 bg-black/18 px-4 py-2 backdrop-blur-md">
      <span className="text-[0.68rem] uppercase tracking-[0.18em] text-white/40">
        {t("pickTimer.label")}
      </span>
      <span
        className={`text-2xl font-bold tabular-nums ${isWarning ? "animate-pulse text-red-400" : "text-white"}`}
      >
        {t("pickTimer.seconds", { count: seconds })}
      </span>
    </div>
  );
}
