import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";

import { useCardDataMeta, formatRelativeDate } from "../../hooks/useCardDataMeta";
import { checkForServiceWorkerUpdate } from "../../pwa/registerServiceWorker";
import { checkForTauriUpdate } from "../../pwa/tauriUpdater";
import { consumeRecentAutoUpdateMarker } from "../../pwa/updateMarker";
import {
  useUpdateStatus,
  useDownloadProgress,
  useUpdateError,
  getUpdateDebugReport,
} from "../../pwa/updateStatus";
import { isTauri } from "../../services/sidecar";

const UPDATED_LABEL_MS = 4500;
const didAutoUpdate = consumeRecentAutoUpdateMarker();

interface BuildBadgeProps {
  className?: string;
  inline?: boolean;
  /** Narrow vertical layout (version + update affordance only) for the rail. */
  compact?: boolean;
}

export function BuildBadge({ className = "", inline = false, compact = false }: BuildBadgeProps = {}) {
  const { t } = useTranslation();
  const [showUpdatedLabel, setShowUpdatedLabel] = useState(didAutoUpdate);
  const cardDataMeta = useCardDataMeta();
  const updateStatus = useUpdateStatus();
  const downloadProgress = useDownloadProgress();
  const updateError = useUpdateError();

  useEffect(() => {
    if (!showUpdatedLabel) return;
    const timeoutId = window.setTimeout(() => setShowUpdatedLabel(false), UPDATED_LABEL_MS);
    return () => window.clearTimeout(timeoutId);
  }, [showUpdatedLabel]);

  const statusLabel = updateStatus === "downloading"
    ? t("buildBadge.downloading", { progress: downloadProgress })
    : updateStatus === "checking"
      ? t("buildBadge.checking")
      : updateStatus === "activating"
        ? t("buildBadge.updating")
        : updateStatus === "deferred"
          ? t("buildBadge.updatePending")
          : null;

  const isActive = updateStatus !== "idle";
  const isDownloading = updateStatus === "downloading";
  const hasUpdateIssue = Boolean(updateError);

  const handleCheckUpdate = () => {
    if (isTauri()) {
      checkForTauriUpdate();
      return;
    }
    checkForServiceWorkerUpdate();
  };

  const handleShowUpdateDebug = () => {
    const report = getUpdateDebugReport();
    window.alert(report);
  };

  const commitUrl = `${__GIT_REPO_URL__}/commit/${__BUILD_HASH__}`;
  const cardDataAge = cardDataMeta ? formatRelativeDate(cardDataMeta.generated_at) : null;
  const cardDataCommitUrl = cardDataMeta
    ? `${__GIT_REPO_URL__}/commit/${cardDataMeta.commit}`
    : null;

  // Compact (rail) layout: a single version + refresh row that fits the 92px
  // rail. Clicking it checks for updates; the build hash and card-data age live
  // in its hover tooltip (the same meta the wide mobile pill shows inline).
  if (compact) {
    const tooltip = [
      `v${__APP_VERSION__} · ${__BUILD_HASH__}`,
      cardDataMeta &&
        t("buildBadge.cardDataTitle", {
          date: cardDataMeta.generated_at,
          commit: cardDataMeta.commit_short,
        }),
      t("buildBadge.checkForUpdates"),
    ]
      .filter(Boolean)
      .join("\n");
    return (
      <div className={`flex flex-col items-center gap-0.5 ${className}`.trim()}>
        <button
          type="button"
          onClick={handleCheckUpdate}
          aria-label={t("buildBadge.checkForUpdates")}
          title={tooltip}
          className="inline-flex items-center gap-1 font-mono text-[10.5px] leading-none text-slate-400 transition-colors hover:text-white"
        >
          <span>v{__APP_VERSION__}</span>
          <span className={`text-[11px] text-slate-500 ${isActive ? "animate-spin" : ""}`} aria-hidden>
            ↻
          </span>
        </button>
        {statusLabel && (
          <span className="text-center text-[8px] leading-tight text-cyan-300">{statusLabel}</span>
        )}
        {hasUpdateIssue && !statusLabel && (
          <button
            type="button"
            onClick={handleShowUpdateDebug}
            className="text-[8px] font-semibold text-rose-300 hover:text-rose-100"
            title={t("buildBadge.updaterIssue", { error: updateError })}
          >
            {t("buildBadge.updateIssue")}
          </button>
        )}
        {showUpdatedLabel && !statusLabel && (
          <span className="text-[8px] text-emerald-300">{t("buildBadge.updated")}</span>
        )}
      </div>
    );
  }

  return (
    <div
      className={inline ? className : `fixed left-2 bottom-[calc(env(safe-area-inset-bottom)+0.5rem)] z-20 ${className}`.trim()}
    >
      <div className="relative flex items-center gap-1 rounded-full border border-white/10 bg-black/18 px-2.5 py-1.5 text-[10px] text-slate-400 shadow-lg shadow-black/30 backdrop-blur-md overflow-hidden">
        <a
          href={commitUrl}
          target="_blank"
          rel="noopener noreferrer"
          className="flex items-center gap-1 transition-colors hover:text-white"
        >
          <span>v{__APP_VERSION__}</span>
          <span className="text-slate-600">{__BUILD_HASH__}</span>
        </a>

        {cardDataMeta && (
          <>
            <span className="text-slate-700">·</span>
            <a
              href={cardDataCommitUrl!}
              target="_blank"
              rel="noopener noreferrer"
              className="text-slate-500 transition-colors hover:text-white"
              title={t("buildBadge.cardDataTitle", {
                date: cardDataMeta.generated_at,
                commit: cardDataMeta.commit_short,
              })}
            >
              {t("buildBadge.cards", { age: cardDataAge, commit: cardDataMeta.commit_short })}
            </a>
          </>
        )}

        <button
          type="button"
          onClick={handleCheckUpdate}
          className={`ml-0.5 text-slate-500 hover:text-white transition-colors cursor-pointer ${isActive ? "animate-spin" : ""}`}
          aria-label={t("buildBadge.checkForUpdates")}
          title={t("buildBadge.checkForUpdates")}
        >
          ↻
        </button>

        {hasUpdateIssue && (
          <button
            type="button"
            onClick={handleShowUpdateDebug}
            className="ml-0.5 rounded px-1 text-[11px] font-semibold text-rose-300 hover:text-rose-100 hover:bg-rose-600/25 transition-colors cursor-pointer"
            aria-label={t("buildBadge.updaterDebugInfo")}
            title={t("buildBadge.updaterIssue", { error: updateError })}
          >
            x
          </button>
        )}

        {statusLabel && <span className="ml-0.5 text-cyan-300">{statusLabel}</span>}
        {hasUpdateIssue && !statusLabel && <span className="ml-0.5 text-rose-300">{t("buildBadge.updateIssue")}</span>}
        {showUpdatedLabel && !statusLabel && <span className="ml-0.5 text-emerald-300">{t("buildBadge.updated")}</span>}

        {isDownloading && (
          <div className="absolute bottom-0 left-0 right-0 h-[2px]">
            <div
              className="h-full bg-cyan-400 transition-[width] duration-200 ease-out"
              style={{ width: `${downloadProgress}%` }}
            />
          </div>
        )}
      </div>
    </div>
  );
}
