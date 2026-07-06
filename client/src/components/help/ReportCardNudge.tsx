import { Trans, useTranslation } from "react-i18next";

import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";

/**
 * First-run nudge that points out the card-report affordance (the red flag in
 * the top-left game menu). Lets a player flag a card that renders or behaves
 * wrong so the parser/engine gap gets telemetry. Mirrors {@link SandboxToolsNudge}:
 * one-time, dismissible, persisted via `preferencesStore.dismissedReportCardNudge`,
 * and sequenced after the sandbox nudge so the first-run hints never stack.
 */
export function ReportCardNudge() {
  const { t } = useTranslation();
  const openCardReportDialog = useUiStore((s) => s.openCardReportDialog);
  const setDismissed = usePreferencesStore((s) => s.setDismissedReportCardNudge);

  return (
    <div className="max-w-[min(24rem,calc(100vw-1.25rem))] rounded-[18px] border border-rose-400/25 bg-slate-950/86 p-3 text-sm text-slate-100 shadow-[0_24px_64px_rgba(15,23,42,0.55)] backdrop-blur-xl">
      <p className="leading-5">
        <Trans
          i18nKey="help.reportCardNudge.message"
          t={t}
          components={{
            flag: <span className="font-semibold text-rose-300" />,
          }}
        />
      </p>
      <div className="mt-3 flex items-center justify-end gap-2">
        <button
          type="button"
          onClick={() => setDismissed(true)}
          className="rounded-lg px-3 py-1.5 text-xs font-semibold text-slate-400 transition hover:bg-white/8 hover:text-slate-200"
        >
          {t("help.reportCardNudge.dismiss")}
        </button>
        <button
          type="button"
          onClick={() => {
            setDismissed(true);
            openCardReportDialog();
          }}
          className="rounded-lg bg-rose-500 px-3 py-1.5 text-xs font-semibold text-white transition hover:bg-rose-400"
        >
          {t("help.reportCardNudge.open")}
        </button>
      </div>
    </div>
  );
}
