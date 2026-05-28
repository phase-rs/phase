import { Trans, useTranslation } from "react-i18next";

import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";

/**
 * First-run nudge that introduces the Sandbox Tools panel (the engine's debug
 * actions). In vs-AI and local games the panel is always live, so a new player
 * can set up any board state — add cards, tokens, counters, life, copy
 * permanents, jump phases. Mirrors {@link FlowHelpNudge}: one-time, dismissible,
 * persisted via `preferencesStore.dismissedSandboxToolsNudge`.
 */
export function SandboxToolsNudge() {
  const { t } = useTranslation();
  const openSandboxTools = useUiStore((s) => s.openSandboxTools);
  const setDismissed = usePreferencesStore((s) => s.setDismissedSandboxToolsNudge);

  return (
    <div className="max-w-[min(24rem,calc(100vw-1.25rem))] rounded-[18px] border border-amber-300/25 bg-slate-950/86 p-3 text-sm text-slate-100 shadow-[0_24px_64px_rgba(15,23,42,0.55)] backdrop-blur-xl">
      <p className="leading-5">
        <Trans
          i18nKey="help.sandboxNudge.message"
          t={t}
          components={{
            tools: <span className="font-semibold text-amber-200" />,
            key: <kbd className="rounded bg-white/10 px-1 font-mono text-xs" />,
          }}
        />
      </p>
      <div className="mt-3 flex items-center justify-end gap-2">
        <button
          type="button"
          onClick={() => setDismissed(true)}
          className="rounded-lg px-3 py-1.5 text-xs font-semibold text-slate-400 transition hover:bg-white/8 hover:text-slate-200"
        >
          {t("help.sandboxNudge.dismiss")}
        </button>
        <button
          type="button"
          onClick={() => {
            setDismissed(true);
            openSandboxTools();
          }}
          className="rounded-lg bg-amber-400 px-3 py-1.5 text-xs font-semibold text-slate-950 transition hover:bg-amber-300"
        >
          {t("help.sandboxNudge.open")}
        </button>
      </div>
    </div>
  );
}
