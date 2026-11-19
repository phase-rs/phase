import type { ParseKeys } from "i18next";
import { useTranslation } from "react-i18next";

import {
  type CardReportContext,
  type CardReportStatus,
  useCardReport,
} from "../../hooks/useCardReport.ts";

// Re-exported for back-compat with existing importers (CardPreview.tsx). The
// type's single source of truth is the hook module; keeping the re-export here
// avoids churning every call site.
export type { CardReportContext };

/** The button's label per status. Only a queued event reads as reported. */
const STATUS_LABEL_KEY = {
  idle: "preview.report",
  sent: "preview.reported",
  notSent: "preview.reportNotSent",
} as const satisfies Record<CardReportStatus, ParseKeys<"game">>;

/** The button's text color per status; the surface supplies everything else. */
const STATUS_COLOR_CLASS = {
  idle: "text-indigo-300 hover:text-indigo-200",
  sent: "text-emerald-400",
  notSent: "text-amber-300 hover:text-amber-200",
} as const satisfies Record<CardReportStatus, string>;

interface ReportCardButtonProps extends CardReportContext {
  /** The hosting surface's shape (size, padding, border), without a text
   *  color: a text link in the desktop parsed-abilities panel, a pill in the
   *  mobile preview's action strip. */
  className: string;
}

/**
 * One-click "report this card" affordance. A single click sends a `card_report`
 * telemetry event — no modal, no confirm step, no free text. The existing
 * GitHub-issue and Discord report flows are untouched; this is the
 * low-friction, identity-free counter. Callers only build a
 * {@link CardReportContext} in a live, participating (non-spectate) game — the
 * guard lives at the call site so the button's styled wrapper elements stay
 * out of the DOM too, not just the button itself.
 *
 * Status is owned by {@link useCardReport}, so the button, the picker rows, and
 * the mobile preview share one authority. A report telemetry dropped reads "Not
 * sent" and stays tappable: a later tap sends it once telemetry is back on.
 * Nothing retries on the player's behalf.
 */
export function ReportCardButton({ className, ...context }: ReportCardButtonProps) {
  const { t } = useTranslation("game");
  const { status, report } = useCardReport(context);

  return (
    <button
      type="button"
      onClick={report}
      disabled={status === "sent"}
      className={`pointer-events-auto ${className} ${STATUS_COLOR_CLASS[status]}`}
    >
      {t(STATUS_LABEL_KEY[status])}
    </button>
  );
}
