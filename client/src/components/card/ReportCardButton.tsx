import { useTranslation } from "react-i18next";

import { type CardReportContext, useCardReport } from "../../hooks/useCardReport.ts";

// Re-exported for back-compat with existing importers (CardPreview.tsx). The
// type's single source of truth is the hook module; keeping the re-export here
// avoids churning every call site.
export type { CardReportContext };

/**
 * One-click "report this card" affordance. A single click sends a `card_report`
 * telemetry event — no modal, no confirm step, no free text. The existing
 * GitHub-issue and Discord report flows are untouched; this is the
 * low-friction, identity-free counter. Callers only build a
 * {@link CardReportContext} in a live, participating (non-spectate) game — the
 * guard lives at the call site so the button's styled wrapper elements stay
 * out of the DOM too, not just the button itself.
 *
 * Dedup/sent state is owned by {@link useCardReport}'s reactive store, so the
 * button, the picker rows, and the mobile pill share one authority.
 */
export function ReportCardButton(props: CardReportContext) {
  const { t } = useTranslation("game");
  const { sent, report } = useCardReport(props);

  return (
    <button
      type="button"
      onClick={report}
      disabled={sent}
      className={
        sent
          ? "pointer-events-auto text-[11px] font-medium text-emerald-400"
          : "pointer-events-auto text-[11px] text-indigo-300 hover:text-indigo-200"
      }
    >
      {sent ? t("preview.reported") : t("preview.report")}
    </button>
  );
}
