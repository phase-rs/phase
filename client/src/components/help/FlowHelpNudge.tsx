import { useTranslation } from "react-i18next";

import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";

export function FlowHelpNudge() {
  const { t } = useTranslation();
  const setHelpSheetOpen = useUiStore((s) => s.setHelpSheetOpen);
  const setDismissed = usePreferencesStore((s) => s.setDismissedFlowHelpNudge);

  return (
    <div className="max-w-[min(24rem,calc(100vw-1.25rem))] rounded-[18px] border border-cyan-300/25 bg-slate-950/86 p-3 text-sm text-slate-100 shadow-[0_24px_64px_rgba(15,23,42,0.55)] backdrop-blur-xl">
      <p className="leading-5">
        {t("help.flowNudge.message")}
      </p>
      <div className="mt-3 flex items-center justify-end gap-2">
        <button
          type="button"
          onClick={() => setDismissed(true)}
          className="rounded-lg px-3 py-1.5 text-xs font-semibold text-slate-400 transition hover:bg-white/8 hover:text-slate-200"
        >
          {t("help.flowNudge.dismiss")}
        </button>
        <button
          type="button"
          onClick={() => {
            setDismissed(true);
            setHelpSheetOpen(true);
          }}
          className="rounded-lg bg-cyan-400 px-3 py-1.5 text-xs font-semibold text-slate-950 transition hover:bg-cyan-300"
        >
          {t("help.flowNudge.learnFlow")}
        </button>
      </div>
    </div>
  );
}
