import { useTranslation } from "react-i18next";

import { useConnectivityStore, useEffectiveOffline } from "../../stores/connectivityStore";

/**
 * Persistent connectivity-policy signal. Local play remains available while
 * offline, but update and online-service lifecycles are deliberately paused.
 */
export function OfflineModeBadge() {
  const { t } = useTranslation();
  const effectiveOffline = useEffectiveOffline();
  const forcedOffline = useConnectivityStore((state) => state.forcedOffline);

  if (!effectiveOffline) return null;

  const reason = t(forcedOffline ? "offlineBadge.forced" : "offlineBadge.connection");

  return (
    <div
      role="status"
      aria-label={`${t("offlineBadge.label")}: ${reason}`}
      title={reason}
      className="pointer-events-none relative z-20 mx-auto mb-2 flex w-fit items-center gap-1.5 rounded-full border border-amber-300/40 bg-slate-950/90 px-3 py-1.5 text-[11px] font-semibold text-amber-100 shadow-lg shadow-black/30 backdrop-blur-md"
    >
      <span className="h-1.5 w-1.5 rounded-full bg-amber-300" aria-hidden />
      <span>{t("offlineBadge.label")}</span>
      <span className="font-normal text-amber-100/75">{t("offlineBadge.servicesPaused")}</span>
    </div>
  );
}
