import { useEffect, useState } from "react";

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
}

export function BuildBadge({ className = "", inline = false }: BuildBadgeProps = {}) {
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
    ? `downloading… ${downloadProgress}%`
    : updateStatus === "checking"
      ? "checking…"
      : updateStatus === "activating"
        ? "updating…"
        : updateStatus === "deferred"
          ? "update pending after game"
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
              title={`Card data generated ${cardDataMeta.generated_at} from ${cardDataMeta.commit_short}`}
            >
              cards {cardDataAge} ({cardDataMeta.commit_short})
            </a>
          </>
        )}

        <button
          type="button"
          onClick={handleCheckUpdate}
          className={`ml-0.5 text-slate-500 hover:text-white transition-colors cursor-pointer ${isActive ? "animate-spin" : ""}`}
          aria-label="Check for updates"
          title="Check for updates"
        >
          ↻
        </button>

        {hasUpdateIssue && (
          <button
            type="button"
            onClick={handleShowUpdateDebug}
            className="ml-0.5 rounded px-1 text-[11px] font-semibold text-rose-300 hover:text-rose-100 hover:bg-rose-600/25 transition-colors cursor-pointer"
            aria-label="Updater debug info"
            title={`Updater issue: ${updateError}`}
          >
            x
          </button>
        )}

        {statusLabel && <span className="ml-0.5 text-cyan-300">{statusLabel}</span>}
        {hasUpdateIssue && !statusLabel && <span className="ml-0.5 text-rose-300">update issue</span>}
        {showUpdatedLabel && !statusLabel && <span className="ml-0.5 text-emerald-300">updated</span>}

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
